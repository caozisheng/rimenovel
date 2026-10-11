//! Narrow call policy; protocols perform exactly one attempt and SQLite owns jobs.
use super::{LlmClient, LlmError, StructuredRequest, StructuredResponse, TextRequest};
use std::{future::Future, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Duration};
use tokio_util::sync::CancellationToken;

pub type CommitGuard<'a> = parking_lot::MutexGuard<'a, ()>;
pub type CallObserver = Arc<dyn Fn(CallObservation) + Send + Sync>;

#[derive(Clone, Debug)]
pub struct CallObservation { pub phase: String, pub attempt: u32, pub retry_after_ms: Option<u64> }

#[derive(Clone, Default)]
pub struct CallControl {
    pub cancellation: CancellationToken,
    pub observer: Option<CallObserver>,
    commit_gate: Arc<parking_lot::Mutex<()>>,
}
impl CallControl {
    pub fn new(cancellation: CancellationToken, observer: Option<CallObserver>) -> Self { Self { cancellation, observer, commit_gate: Arc::default() } }
    pub fn with_observer(mut self, observer: CallObserver) -> Self { self.observer = Some(observer); self }
    pub fn cancel(&self) { let _guard = self.commit_gate.lock(); self.cancellation.cancel(); }
    pub fn check(&self) -> Result<(), LlmError> { if self.cancellation.is_cancelled() { Err(LlmError::Cancelled) } else { Ok(()) } }
    pub fn commit_guard(&self) -> Result<CommitGuard<'_>, LlmError> {
        let guard = self.commit_gate.lock();
        self.check()?;
        Ok(guard)
    }
    fn observe(&self, phase: &str, attempt: u32, delay: Option<Duration>) {
        if let Some(observer) = &self.observer { observer(CallObservation { phase: phase.into(), attempt, retry_after_ms: delay.map(|value| value.as_millis().min(u64::MAX as u128) as u64) }); }
    }
}

const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(180);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_ATTEMPTS: u32 = 3;

fn retryable(error: &LlmError) -> bool {
    matches!(error, LlmError::Network(_) | LlmError::Timeout | LlmError::Server { status: 408 | 429 | 500 | 502 | 503 | 504, .. })
}
fn retry_delay(error: &LlmError, attempt: u32) -> Duration {
    if let LlmError::Server { retry_after: Some(delay), .. } = error { return (*delay).min(Duration::from_secs(30)); }
    let base = 1000u64.saturating_mul(1u64 << attempt.saturating_sub(1).min(4));
    Duration::from_millis((base + rand::random_range(0..=250u64)).min(30_000))
}

pub struct PolicyClient { inner: Box<dyn LlmClient>, control: CallControl, secret: String }
impl PolicyClient {
    pub fn new(inner: Box<dyn LlmClient>, control: CallControl) -> Self { Self { inner, control, secret: String::new() } }
    pub fn with_secret(mut self, secret: String) -> Self { self.secret = secret; self }
    fn sanitize(&self, error: LlmError) -> LlmError {
        if self.secret.is_empty() { return error; }
        let clean = |text: String| text.replace(&self.secret, "[redacted]");
        match error {
            LlmError::Server { status, body, retry_after, request_id } => LlmError::Server { status, body: clean(body), retry_after, request_id: request_id.map(clean) },
            LlmError::Network(text) => LlmError::Network(clean(text)),
            LlmError::Schema(text) => LlmError::Schema(clean(text)),
            LlmError::Stream(text) => LlmError::Stream(clean(text)),
            LlmError::Config(text) => LlmError::Config(clean(text)),
            other => other,
        }
    }
    async fn run<T, F, Fut>(&self, phase: &str, mut operation: F, emitted: &AtomicBool) -> Result<T, LlmError>
    where F: FnMut() -> Fut, Fut: Future<Output = Result<T, LlmError>> {
        let total_deadline = tokio::time::Instant::now() + TOTAL_TIMEOUT;
        for attempt in 1..=MAX_ATTEMPTS {
            self.control.check()?;
            self.control.observe(phase, attempt, None);
            let attempt_deadline = total_deadline.min(tokio::time::Instant::now() + ATTEMPT_TIMEOUT);
            let result = tokio::select! {
                biased;
                _ = self.control.cancellation.cancelled() => return Err(LlmError::Cancelled),
                result = tokio::time::timeout_at(attempt_deadline, operation()) => result.unwrap_or(Err(LlmError::Timeout)),
            };
            match result {
                Ok(value) => { self.control.check()?; return Ok(value); }
                Err(error) => {
                    if attempt == MAX_ATTEMPTS || emitted.load(Ordering::Acquire) || !retryable(&error) { return Err(self.sanitize(error)); }
                    let now = tokio::time::Instant::now();
                    if now >= total_deadline { return Err(LlmError::Timeout); }
                    let delay = retry_delay(&error, attempt);
                    self.control.observe("retry_wait", attempt, Some(delay));
                    tokio::select! {
                        biased;
                        _ = self.control.cancellation.cancelled() => return Err(LlmError::Cancelled),
                        _ = tokio::time::sleep_until(total_deadline) => return Err(LlmError::Timeout),
                        _ = tokio::time::sleep(delay) => {},
                    }
                }
            }
        }
        unreachable!("bounded loop always returns on last attempt")
    }
}
#[async_trait::async_trait]
impl LlmClient for PolicyClient {
    async fn structured_with_usage(&self, req: StructuredRequest<'_>) -> Result<StructuredResponse, LlmError> {
        if req.max_tokens == 0 { return Err(LlmError::Config("输出预算必须大于零".into())); }
        if req.schema_name == "kg_chapter" {
            self.control.check()?;
            self.control.observe("extract", 1, None);
            let result = tokio::select! {
                biased;
                _ = self.control.cancellation.cancelled() => Err(LlmError::Cancelled),
                result = tokio::time::timeout(crate::core::kg_extract::REQUEST_TIMEOUT, self.inner.structured_with_usage(req)) => result.unwrap_or(Err(LlmError::Timeout)),
            };
            self.control.check()?;
            return result.map_err(|error| self.sanitize(error));
        }
        let phase = if req.schema_name == "kg_working" { "reconcile" } else { "request" };
        self.run(phase, || self.inner.structured_with_usage(req.clone()), &AtomicBool::new(false)).await
    }
    async fn stream_text(&self, req: TextRequest, on_delta: Arc<dyn for<'a> Fn(&'a str) + Send + Sync>) -> Result<String, LlmError> {
        if req.max_tokens.is_none_or(|value| value == 0) { return Err(LlmError::Config("正文输出预算必须明确且大于零".into())); }
        let emitted = Arc::new(AtomicBool::new(false));
        let mark = emitted.clone();
        let cancellation = self.control.cancellation.clone();
        let callback: Arc<dyn for<'a> Fn(&'a str) + Send + Sync> = Arc::new(move |delta| {
            if !delta.is_empty() && !cancellation.is_cancelled() { mark.store(true, Ordering::Release); on_delta(delta); }
        });
        self.run("write", || self.inner.stream_text(req.clone(), callback.clone()), &emitted).await
    }
    fn check_cancelled(&self) -> Result<(), LlmError> { self.control.check() }
    fn commit_guard(&self) -> Result<Option<CommitGuard<'_>>, LlmError> {
        self.control.commit_guard().map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct Scripted { attempts: Arc<AtomicUsize>, status: u16, emit: bool }
    #[async_trait::async_trait]
    impl LlmClient for Scripted {
        async fn structured_with_usage(&self, _: StructuredRequest<'_>) -> Result<StructuredResponse, LlmError> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            Err(LlmError::Server { status:self.status, body:"upstream".into(), retry_after:Some(Duration::ZERO), request_id:None })
        }
        async fn stream_text(&self, _:TextRequest, delta:Arc<dyn for<'a> Fn(&'a str)+Send+Sync>) -> Result<String,LlmError> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            if self.emit { delta("半句"); }
            Err(LlmError::Server {status:self.status,body:"upstream".into(),retry_after:Some(Duration::ZERO),request_id:None})
        }
    }
    fn request(schema:&serde_json::Value)->StructuredRequest<'_> { StructuredRequest {task:super::super::TaskKind::Extract,messages:vec![],schema,schema_name:"test",temperature:0.0,max_tokens:128} }
    #[tokio::test]
    async fn compact_extraction_never_spends_quota_on_automatic_retries() {
        let attempts=Arc::new(AtomicUsize::new(0));
        let client=PolicyClient::new(Box::new(Scripted {attempts:attempts.clone(),status:503,emit:false}),CallControl::default());
        let schema=json!({"type":"object"});
        let mut req=request(&schema);req.schema_name="kg_chapter";
        assert!(matches!(client.structured_with_usage(req).await,Err(LlmError::Server {status:503,..})));
        assert_eq!(attempts.load(Ordering::SeqCst),1);
    }
    #[tokio::test]
    async fn server_errors_are_bounded_and_auth_is_not_retried() {
        for (status,expected) in [(503,3),(504,3),(429,3),(401,1),(400,1)] {
            let attempts=Arc::new(AtomicUsize::new(0));
            let client=PolicyClient::new(Box::new(Scripted {attempts:attempts.clone(),status,emit:false}),CallControl::default());
            let schema=json!({"type":"object"});
            assert!(matches!(client.structured_with_usage(request(&schema)).await,Err(LlmError::Server {status:s,..}) if s==status));
            assert_eq!(attempts.load(Ordering::SeqCst),expected);
        }
    }
    #[tokio::test]
    async fn a_partial_stream_is_never_replayed() {
        let attempts=Arc::new(AtomicUsize::new(0));
        let client=PolicyClient::new(Box::new(Scripted {attempts:attempts.clone(),status:503,emit:true}),CallControl::default());
        let output=Arc::new(parking_lot::Mutex::new(String::new()));
        let sink=output.clone();
        let req=TextRequest {task:super::super::TaskKind::Write,messages:vec![],temperature:0.8,max_tokens:Some(128)};
        assert!(client.stream_text(req,Arc::new(move|delta|sink.lock().push_str(delta))).await.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst),1);
        assert_eq!(&*output.lock(),"半句");
    }
    #[tokio::test]
    async fn cancellation_interrupts_retry_wait_and_blocks_commit() {
        let control=CallControl::default();
        let cancel=control.clone();
        let observations=Arc::new(parking_lot::Mutex::new(Vec::new()));
        let out=observations.clone();
        let control=control.with_observer(Arc::new(move |event:CallObservation| { out.lock().push(event.phase.clone()); if event.phase=="retry_wait" {cancel.cancel();} }));
        let attempts=Arc::new(AtomicUsize::new(0));
        let client=PolicyClient::new(Box::new(Scripted {attempts:attempts.clone(),status:503,emit:false}),control);
        let schema=json!({"type":"object"});
        assert!(matches!(client.structured_with_usage(request(&schema)).await,Err(LlmError::Cancelled)));
        assert_eq!(attempts.load(Ordering::SeqCst),1);
        assert!(client.commit_guard().is_err());
        assert_eq!(*observations.lock(),vec!["request","retry_wait"]);
    }
    #[test]
    fn retry_after_is_bounded_and_http_date_is_parsed() {
        let mut headers=reqwest::header::HeaderMap::new();
        headers.insert("retry-after","6000".parse().unwrap());
        headers.insert("x-request-id","provider-123".parse().unwrap());
        let error=super::super::server_error(503,"busy".into(),&headers);
        assert_eq!(retry_delay(&error,1),Duration::from_secs(30));
        headers.insert("retry-after",httpdate::fmt_http_date(std::time::SystemTime::now()+Duration::from_secs(10)).parse().unwrap());
        let error=super::super::server_error(503,"busy".into(),&headers);
        assert!(matches!(&error,LlmError::Server {retry_after:Some(delay),request_id:Some(id),..} if *delay<=Duration::from_secs(10)&&*delay>=Duration::from_secs(8)&&id=="provider-123"));
    }

    #[tokio::test(start_paused = true)]
    async fn provider_hang_has_a_finite_deadline_and_attempt_count() {
        let attempts=Arc::new(AtomicUsize::new(0));
        let seen=attempts.clone();
        let control=CallControl::default().with_observer(Arc::new(move |event| { if event.phase=="request" {seen.fetch_add(1,Ordering::SeqCst);} }));
        let client=PolicyClient::new(Box::new(Scripted {attempts:Arc::new(AtomicUsize::new(0)),status:503,emit:false}),control);
        let result=client.run("request",std::future::pending::<Result<(),LlmError>>,&AtomicBool::new(false)).await;
        assert!(matches!(result,Err(LlmError::Timeout)));
        assert_eq!(attempts.load(Ordering::SeqCst),3);
    }

    #[tokio::test]
    async fn cancellation_interrupts_an_inflight_request() {
        let control=CallControl::default();
        let cancel=control.clone();
        let client=PolicyClient::new(Box::new(Scripted {attempts:Arc::new(AtomicUsize::new(0)),status:503,emit:false}),control);
        let result=client.run("request",|| { cancel.cancel(); std::future::pending::<Result<(),LlmError>>() },&AtomicBool::new(false)).await;
        assert!(matches!(result,Err(LlmError::Cancelled)));
    }

    #[tokio::test]
    async fn echoed_access_token_is_not_returned_to_the_job() {
        let client=PolicyClient::new(Box::new(Scripted {attempts:Arc::new(AtomicUsize::new(0)),status:401,emit:false}),CallControl::default()).with_secret("upstream".into());
        let schema=json!({"type":"object"});
        let error=client.structured_with_usage(request(&schema)).await.unwrap_err();
        assert_eq!(error.to_string(),"限流/服务端错误 401: [redacted]");
    }
}

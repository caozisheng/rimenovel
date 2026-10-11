// Remove consecutive leading UTF-8 BOMs before the SDK's SSE decoder.
// eventsource-stream 0.2.3 slices U+FEFF at byte 1 and panics; no event framing or
// JSON parsing belongs here. After the leading BOMs, upstream bytes pass through.

use async_openai::error::OpenAIError;
use bytes::{Buf, Bytes};
use futures_util::{Stream, StreamExt};
use reqwest::ResponseBuilderExt;
use std::pin::Pin;

static UTF8_BOM: [u8; 3] = [0xef, 0xbb, 0xbf];

struct PrefixState<S> {
    upstream: Pin<Box<S>>,
    matched: usize,
    checked: bool,
    pending: Option<Bytes>,
    cancellation: tokio_util::sync::CancellationToken,
}

pub(super) fn strip_prefix<S, E>(upstream: S, cancellation: tokio_util::sync::CancellationToken) -> impl Stream<Item = Result<Bytes, E>> + Send
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: Send + 'static,
{
    let state = PrefixState {
        upstream: Box::pin(upstream),
        matched: 0,
        checked: false,
        pending: None,
        cancellation,
    };
    futures_util::stream::unfold(state, |mut state| async move {
        if let Some(bytes) = state.pending.take() {
            return Some((Ok(bytes), state));
        }
        loop {
            let next = tokio::select! {
                biased;
                _ = state.cancellation.cancelled() => return None,
                next = state.upstream.next() => next,
            };
            match next {
                Some(Ok(mut bytes)) => {
                    if state.checked {
                        return Some((Ok(bytes), state));
                    }
                    while !bytes.is_empty() {
                        if bytes[0] != UTF8_BOM[state.matched] {
                            state.checked = true;
                            if state.matched == 0 {
                                return Some((Ok(bytes), state));
                            }
                            // A partial BOM was actually ordinary payload. Restore
                            // its known bytes without allocating or copying chunks.
                            state.pending = Some(bytes);
                            let prefix = Bytes::from_static(&UTF8_BOM[..state.matched]);
                            return Some((Ok(prefix), state));
                        }
                        state.matched += 1;
                        bytes.advance(1);
                        if state.matched == UTF8_BOM.len() {
                            // Keep checking until real payload starts: forwarding
                            // another leading BOM would trigger the same SDK panic.
                            state.matched = 0;
                        }
                    }
                }
                Some(Err(error)) => return Some((Err(error), state)),
                None if !state.checked && state.matched > 0 => {
                    state.checked = true;
                    let prefix = Bytes::from_static(&UTF8_BOM[..state.matched]);
                    return Some((Ok(prefix), state));
                }
                None => return None,
            }
        }
    })
}

pub(super) fn normalize(mut response: reqwest::Response, cancellation: tokio_util::sync::CancellationToken) -> Result<reqwest::Response, OpenAIError> {
    // The SDK parses SSE based on the requested API, even if upstream omits or
    // mislabels Content-Type. Only request intent reliably protects its decoder.
    let status = response.status();
    let version = response.version();
    let url = response.url().clone();
    let mut headers = std::mem::take(response.headers_mut());
    let extensions = std::mem::take(response.extensions_mut());
    // Leading BOM removal changes the body length; its size is now streaming.
    headers.remove(reqwest::header::CONTENT_LENGTH);
    let body = reqwest::Body::wrap_stream(strip_prefix(response.bytes_stream(), cancellation));
    let mut normalized = http::Response::builder().url(url).body(body)
        .map_err(|error| OpenAIError::Boxed(Box::new(error)))?;
    *normalized.status_mut() = status;
    *normalized.version_mut() = version;
    *normalized.headers_mut() = headers;
    normalized.extensions_mut().extend(extensions);
    Ok(normalized.into())
}

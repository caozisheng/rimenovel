use super::*;
use crate::store::{books::{create_book_with_chapters,NewChapter},Db};
use crate::core::quick_read::{ChapterQuickRead,QuickReadSlice,SourceSpan,QuickReadCharacterMention,QuickReadReference,QuickReadCharacterCard,SourceChapter};
use rusqlite::Connection;

fn fixture()->(Connection,i64,Vec<SourceChapter>){
    let conn=Connection::open_in_memory().unwrap();Db::apply_pragmas(&conn).unwrap();Db::new(&conn).migrate().unwrap();
    let chapters=vec![NewChapter{idx:1,title:"One".into(),text:"阿七路过".into(),token_est:5},NewChapter{idx:2,title:"Two".into(),text:"顾岚回归".into(),token_est:5}];
    let book=create_book_with_chapters(&conn,"QuickRead",None,"txt","test",&chapters).unwrap();
    let sources=source_chapters(&conn,book).unwrap();(conn,book,sources)
}
fn slice(idx:usize,source:&SourceChapter)->QuickReadSlice{
    QuickReadSlice{slice_idx:idx,source_spans:vec![SourceSpan{chapter_idx:source.chapter_idx,start_char:0,end_char:source.char_count,content_hash:source.content_hash.clone()}],chapter_summaries:vec![ChapterQuickRead{chapter_idx:source.chapter_idx,summary:format!("Chapter {}",source.chapter_idx),character_refs:vec![format!("s{idx}:c1")]}],characters:vec![QuickReadCharacterMention{local_id:format!("s{idx}:c1"),name:"阿七".into(),aliases:vec![],chapter_idx:source.chapter_idx,evidence:"本章出现".into()}],plot_notes:vec![]}
}
fn plan(sources:&[SourceChapter])->Vec<Vec<SourceSpan>> {sources.iter().map(|s|vec![SourceSpan{chapter_idx:s.chapter_idx,start_char:0,end_char:s.char_count,content_hash:s.content_hash.clone()}]).collect()}
fn ready(sources:&[SourceChapter])->QuickReadReference {QuickReadReference{schema_version:"v1".into(),source_digest:source_digest(sources),source_kind:crate::core::quick_read::QuickReadSourceKind::LocalFullText,provider_id:1,model:"reader".into(),prompt_version:"v1".into(),chapter_summaries:sources.iter().map(|s|ChapterQuickRead{chapter_idx:s.chapter_idx,summary:format!("Chapter {}",s.chapter_idx),character_refs:vec![format!("s{}:c1",(s.chapter_idx-1) as usize)]}).collect(),characters:vec![QuickReadCharacterCard{id:"c".into(),name:"阿七".into(),aliases:vec![],identity_evidence:vec!["文本有据".into()],motivations:vec!["保护同伴".into()],narrative_roles:vec!["主线人物".into()],first_appearance_chapter:1,revelations:vec![],source_chapters:vec![1]}],plot_summary:"主线".into(),plot_notes:vec![crate::core::quick_read::QuickReadPlotNote{summary:"身份揭示".into(),source_chapters:vec![1,2]}],completed_spans:sources.iter().map(|s|SourceSpan{chapter_idx:s.chapter_idx,start_char:0,end_char:s.char_count,content_hash:s.content_hash.clone()}).collect(),source_provenance:vec![]}}

#[test]
fn checkpointed_slice_survives_reopen_and_only_missing_work_remains(){
    let (conn,book,sources)=fixture();let dependency="provider:reader:v1";
    let revision=prepare_revision(&conn,book,&sources,dependency,1,"reader","v1","v1",&plan(&sources)).unwrap();
    save_slice(&conn,revision,&slice(0,&sources[0]),(100,20)).unwrap();
    assert_eq!(pending_slices(&conn,revision).unwrap(),vec![1]);
    let reused=prepare_revision(&conn,book,&sources,dependency,1,"reader","v1","v1",&plan(&sources)).unwrap();
    assert_eq!(reused,revision);
    assert_eq!(load_slices(&conn,revision).unwrap().len(),1);
}

#[test]
fn failed_slice_keeps_successful_checkpoint_and_retries_only_unfinished_work(){
    let (conn,book,sources)=fixture();
    let revision=prepare_revision(&conn,book,&sources,"provider:reader:v1",1,"reader","v1","v1",&plan(&sources)).unwrap();
    save_slice(&conn,revision,&slice(0,&sources[0]),(100,20)).unwrap();
    mark_failed(&conn,revision,"请求失败，请重试速读").unwrap();
    assert_eq!(pending_slices(&conn,revision).unwrap(),vec![1]);
    assert_eq!(load_slices(&conn,revision).unwrap().len(),1);
    assert_eq!(prepare_revision(&conn,book,&sources,"provider:reader:v1",1,"reader","v1","v1",&plan(&sources)).unwrap(),revision);
}

#[test]
fn a_new_source_version_gets_a_new_reference_and_does_not_reuse_old_slices(){
    let (conn,book,sources)=fixture();let old=prepare_revision(&conn,book,&sources,"v1",1,"reader","v1","v1",&plan(&sources)).unwrap();
    save_slice(&conn,old,&slice(0,&sources[0]),(100,20)).unwrap();save_slice(&conn,old,&slice(1,&sources[1]),(100,20)).unwrap();
    publish_reference(&conn,old,&ready(&sources)).unwrap();assert!(active_reference(&conn,book).unwrap().is_some());
    let changed_book=create_book_with_chapters(&conn,"QuickRead v2",None,"txt","test-v2",&[NewChapter{idx:1,title:"One".into(),text:"阿七路过".into(),token_est:5},NewChapter{idx:2,title:"Two".into(),text:"顾岚改变结局".into(),token_est:6}]).unwrap();
    let changed=source_chapters(&conn,changed_book).unwrap();let fresh=prepare_revision(&conn,changed_book,&changed,"v1",1,"reader","v1","v1",&plan(&changed)).unwrap();
    assert_ne!(source_digest(&sources),source_digest(&changed));assert_ne!(fresh,old);assert_eq!(pending_slices(&conn,fresh).unwrap(),vec![0,1]);
    assert!(active_reference(&conn,changed_book).unwrap().is_none());
}

#[test]
fn incomplete_slices_cannot_publish_ready_even_if_summary_was_returned(){
    let (conn,book,sources)=fixture();let revision=prepare_revision(&conn,book,&sources,"v1",1,"reader","v1","v1",&plan(&sources)).unwrap();
    save_slice(&conn,revision,&slice(0,&sources[0]),(100,20)).unwrap();
    let error=publish_reference(&conn,revision,&ready(&sources)).unwrap_err();
    assert!(error.contains("incomplete"));assert!(active_reference(&conn,book).unwrap().is_none());
}

#[test]
fn book_delete_cascades_ready_reference_and_chunks(){
    let (conn,book,sources)=fixture();let revision=prepare_revision(&conn,book,&sources,"v1",1,"reader","v1","v1",&plan(&sources)).unwrap();
    save_slice(&conn,revision,&slice(0,&sources[0]),(100,20)).unwrap();save_slice(&conn,revision,&slice(1,&sources[1]),(100,20)).unwrap();
    publish_reference(&conn,revision,&ready(&sources)).unwrap();
    crate::store::books::delete_book(&conn,book).unwrap();
    assert!(active_reference(&conn,book).unwrap().is_none());
}

#[test]
fn resume_uses_original_span_plan_when_provider_context_changes(){
 let (conn,book,sources)=fixture();let original=plan(&sources);
 let revision=prepare_revision(&conn,book,&sources,"fixed-provider-model",1,"reader","quick-read/v1","quick-read-summary/v1",&original).unwrap();
 save_slice(&conn,revision,&slice(0,&sources[0]),(100,20)).unwrap();
 let resized=vec![original.iter().flatten().cloned().collect::<Vec<_>>()];
 assert_eq!(prepare_revision(&conn,book,&sources,"fixed-provider-model",1,"reader","quick-read/v1","quick-read-summary/v1",&resized).unwrap(),revision);
 assert_eq!(slice_plan(&conn,revision).unwrap(),original);assert_eq!(pending_slices(&conn,revision).unwrap(),vec![1]);
}

#[test]
fn legacy_slice_schema_metadata_is_corrected_without_discarding_checkpoints(){
 let (conn,book,sources)=fixture();let legacy=serde_json::to_string(crate::core::quick_read::slice_schema()).unwrap();
 let revision=prepare_revision(&conn,book,&sources,"legacy-worker",1,"reader","quick-read/v1",&legacy,&plan(&sources)).unwrap();
 save_slice(&conn,revision,&slice(0,&sources[0]),(100,20)).unwrap();
 assert_eq!(prepare_revision(&conn,book,&sources,"legacy-worker",1,"reader","quick-read/v1","quick-read-summary/v1",&plan(&sources)).unwrap(),revision);
 assert_eq!(conn.query_row("SELECT schema_version FROM quick_read_revisions WHERE id=?1",[revision],|row|row.get::<_,String>(0)).unwrap(),"quick-read-summary/v1");
 assert_eq!(load_slices(&conn,revision).unwrap(),vec![slice(0,&sources[0])]);
}

#[test]
fn publication_validates_provenance_against_real_source_indices_not_chapter_count(){
 let (conn,_,_)=fixture();
 let chapters=vec![NewChapter{idx:10,title:"Ten".into(),text:"阿七路过".into(),token_est:5},NewChapter{idx:20,title:"Twenty".into(),text:"顾岚回归".into(),token_est:5}];
 let book=create_book_with_chapters(&conn,"Sparse chapters",None,"txt","sparse-test",&chapters).unwrap();let sources=source_chapters(&conn,book).unwrap();
 let revision=prepare_revision(&conn,book,&sources,"v1",1,"reader","v1","v1",&plan(&sources)).unwrap();
 let slices=vec![slice(0,&sources[0]),slice(1,&sources[1])];for value in &slices {save_slice(&conn,revision,value,(10,10)).unwrap();}
 let mut reference=ready(&sources);reference.chapter_summaries=crate::core::quick_read::canonical_chapter_summaries(&slices);
 reference.characters[0].first_appearance_chapter=10;reference.characters[0].source_chapters=vec![10,20];reference.characters[0].revelations=vec![crate::core::quick_read::CharacterReveal{chapter_idx:20,evidence:"身份揭示".into()}];reference.plot_notes[0].source_chapters=vec![10,20];
 publish_reference(&conn,revision,&reference).unwrap();assert_eq!(active_reference(&conn,book).unwrap(),Some(reference));
}

fn web_ready(sources:&[SourceChapter])->QuickReadReference {QuickReadReference{schema_version:"web-outline/v1".into(),source_digest:source_digest(sources),source_kind:crate::core::quick_read::QuickReadSourceKind::WebOutline,provider_id:1,model:"reader".into(),prompt_version:"web-outline/v1".into(),chapter_summaries:sources.iter().map(|source|ChapterQuickRead{chapter_idx:source.chapter_idx,summary:format!("Web chapter {}",source.chapter_idx),character_refs:if source.chapter_idx==1{vec!["web-c".into()]}else{vec![]}}).collect(),characters:vec![QuickReadCharacterCard{id:"web-c".into(),name:"阿七".into(),aliases:vec![],identity_evidence:vec!["网页人物基线".into()],motivations:vec![],narrative_roles:vec!["web_outline".into()],first_appearance_chapter:1,revelations:vec![],source_chapters:vec![1]}],plot_summary:"网页主线".into(),plot_notes:vec![crate::core::quick_read::QuickReadPlotNote{summary:"网页主线".into(),source_chapters:vec![1]}],completed_spans:vec![],source_provenance:vec![crate::web_search::WebOutlineProvenance{url:"https://example.com/outline".into(),title:"Outline".into(),evidence:"足够长的网页佐证文字".into()}]}}
fn ai_proof(reference:&QuickReadReference,chapter_index:usize)->crate::web_search::EditionProof {
    crate::web_search::EditionProof{chapter_index,method:crate::web_search::EditionProofMethod::AiParagraph,provenance:reference.source_provenance[0].clone()}
}

#[test]
fn web_reference_is_zero_slice_and_never_exposes_local_spans(){
 let (conn,book,sources)=fixture();let reference=web_ready(&sources);let map=vec![(1,"One".into(),"https://example.com/outline".into()),(2,"Two".into(),"https://example.com/outline".into())];
 let revision=publish_web_reference(&conn,book,"QuickRead",&ai_proof(&reference,2),"web-dependency",&map,&reference).unwrap();
 assert_eq!(conn.query_row("SELECT total_chars,completed_chars,total_slices,completed_slices FROM quick_read_revisions WHERE id=?1",[revision],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?,row.get::<_,i64>(3)?))).unwrap(),(0,0,0,0));
 assert_eq!(conn.query_row("SELECT proof_kind,proof_chapter_idx,match_score FROM quick_read_web_outlines WHERE revision_id=?1",[revision],|row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,Option<f64>>(2)?))).unwrap(),("ai_paragraph".into(),2,None));
 assert!(conn.execute("UPDATE quick_read_web_outlines SET match_score=0.99 WHERE revision_id=?1",[revision]).is_err());
 assert!(conn.execute("UPDATE quick_read_web_outlines SET proof_kind='unverified' WHERE revision_id=?1",[revision]).is_err());
 assert!(conn.execute("UPDATE quick_read_web_outlines SET proof_chapter_idx=0 WHERE revision_id=?1",[revision]).is_err());
 let active=active_reference(&conn,book).unwrap().unwrap();assert_eq!(active,reference);assert!(active.completed_spans.is_empty());
 conn.execute("DELETE FROM quick_read_web_outline_chapters WHERE revision_id=?1 AND chapter_idx=2",[revision]).unwrap();assert!(active_reference(&conn,book).unwrap().is_none());
}

#[test]
fn web_publication_rejects_missing_provenance_or_incomplete_chapters(){
 let (conn,book,sources)=fixture();let mut reference=web_ready(&sources);let proof=ai_proof(&reference,2);let map=vec![(1,"One".into(),"https://example.com/outline".into())];
 assert!(publish_web_reference(&conn,book,"QuickRead",&proof,"web-dependency",&map,&reference).unwrap_err().contains("incomplete"));
 reference.source_provenance.clear();let map=vec![(1,"One".into(),"https://example.com/outline".into()),(2,"Two".into(),"https://example.com/outline".into())];
 assert!(publish_web_reference(&conn,book,"QuickRead",&proof,"web-dependency",&map,&reference).unwrap_err().contains("provenance"));
}

#[test]
fn source_change_stales_a_published_web_reference(){
 let (conn,book,sources)=fixture();let reference=web_ready(&sources);let map=vec![(1,"One".into(),"https://example.com/outline".into()),(2,"Two".into(),"https://example.com/outline".into())];
 publish_web_reference(&conn,book,"QuickRead",&ai_proof(&reference,2),"web-dependency",&map,&reference).unwrap();
 conn.execute("INSERT INTO chapters(book_id,idx,title,original_text,token_est) VALUES (?1,3,'Three','new source chapter',3)",[book]).unwrap();assert!(active_reference(&conn,book).unwrap().is_none());
}

#[test]
fn local_fallback_revives_matching_checkpoints_after_web_reference(){
 let (conn,book,sources)=fixture();let dependency="local-reader-v1";let revision=prepare_revision(&conn,book,&sources,dependency,1,"reader","v1","v1",&plan(&sources)).unwrap();
 save_slice(&conn,revision,&slice(0,&sources[0]),(10,10)).unwrap();
 let reference=web_ready(&sources);let map=vec![(1,"One".into(),"https://example.com/outline".into()),(2,"Two".into(),"https://example.com/outline".into())];
 publish_web_reference(&conn,book,"QuickRead",&ai_proof(&reference,2),"web-dependency",&map,&reference).unwrap();
 let revived=prepare_revision(&conn,book,&sources,dependency,1,"reader","v1","v1",&plan(&sources)).unwrap();
 assert_eq!(revived,revision);assert_eq!(load_slices(&conn,revived).unwrap(),vec![slice(0,&sources[0])]);assert_eq!(pending_slices(&conn,revived).unwrap(),vec![1]);
}

#[test]
fn invalid_edition_proof_cannot_replace_active_reference(){
 let (conn,book,sources)=fixture();let reference=web_ready(&sources);let map=vec![(1,"One".into(),"https://example.com/outline".into()),(2,"Two".into(),"https://example.com/outline".into())];
 let revision=publish_web_reference(&conn,book,"QuickRead",&ai_proof(&reference,2),"web-dependency",&map,&reference).unwrap();
 let mut proof=ai_proof(&reference,2);proof.method=crate::web_search::EditionProofMethod::TextSimilarity;
 assert!(publish_web_reference(&conn,book,"QuickRead",&proof,"web-dependency",&map,&reference).is_err());
 proof=ai_proof(&reference,3);assert!(publish_web_reference(&conn,book,"QuickRead",&proof,"web-dependency",&map,&reference).is_err());
 proof=ai_proof(&reference,2);proof.provenance.url="https://example.com/unaccepted".into();assert!(publish_web_reference(&conn,book,"QuickRead",&proof,"web-dependency",&map,&reference).is_err());
 let mut mismatched_reference=reference.clone();mismatched_reference.source_provenance.push(proof.provenance.clone());
 assert!(publish_web_reference(&conn,book,"QuickRead",&proof,"web-dependency",&map,&mismatched_reference).is_err());
 assert!(publish_web_reference(&conn,book,"QuickRead",&ai_proof(&reference,2),"web-dependency",&map[..1],&reference).is_err());
 assert_eq!(active_reference(&conn,book).unwrap(),Some(reference));
 assert_eq!(conn.query_row("SELECT id FROM quick_read_revisions WHERE book_id=?1 AND active=1",[book],|row|row.get::<_,i64>(0)).unwrap(),revision);
}

#[test]
fn ai_proof_with_fake_numeric_score_or_unmapped_chapter_is_not_qualified(){
 let (conn,book,sources)=fixture();let reference=web_ready(&sources);let map=vec![(1,"One".into(),"https://example.com/outline".into()),(2,"Two".into(),"https://example.com/outline".into())];
 let revision=publish_web_reference(&conn,book,"QuickRead",&ai_proof(&reference,2),"web-dependency",&map,&reference).unwrap();
 conn.execute_batch("PRAGMA ignore_check_constraints=ON").unwrap();
 conn.execute("UPDATE quick_read_web_outlines SET match_score=1.0 WHERE revision_id=?1",[revision]).unwrap();assert!(active_reference(&conn,book).is_err());
 conn.execute("UPDATE quick_read_web_outlines SET match_score=NULL,proof_chapter_idx=3 WHERE revision_id=?1",[revision]).unwrap();assert!(active_reference(&conn,book).unwrap().is_none());
}

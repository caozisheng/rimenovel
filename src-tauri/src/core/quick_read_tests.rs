use super::*;
use serde_json::json;

fn source(chapter_idx:i64,char_count:usize)->SourceChapter{
    SourceChapter{chapter_idx,char_count,content_hash:format!("chapter-{chapter_idx}")}
}
fn slice(slice_idx:usize, chapter_idx:i64,start:usize,end:usize)->QuickReadSlice{
    QuickReadSlice{slice_idx,source_spans:vec![SourceSpan{chapter_idx,start_char:start,end_char:end,content_hash:format!("chapter-{chapter_idx}")}],chapter_summaries:vec![ChapterQuickRead{chapter_idx,summary:format!("chapter {chapter_idx}"),character_refs:vec![]}],characters:vec![],plot_notes:vec![]}
}

#[test]
fn full_coverage_accepts_exact_adjacent_spans_and_all_chapter_summaries(){
    let sources=vec![source(1,5),source(2,3)];
    let mut second=slice(1,1,3,5);second.chapter_summaries.clear();
    let slices=vec![slice(0,1,0,3),second,slice(2,2,0,3)];
    assert!(validate_quick_read_coverage(&sources,&slices).is_ok());
}

#[test]
fn missing_middle_source_span_cannot_mark_full_book_ready(){
    let sources=vec![source(1,5)];
    let slices=vec![slice(0,1,0,2),slice(1,1,3,5)];
    assert!(validate_quick_read_coverage(&sources,&slices).unwrap_err().contains("coverage"));
}

#[test]
fn overlapping_source_span_cannot_mark_full_book_ready(){
    let sources=vec![source(1,5)];
    let mut second=slice(1,1,2,5);second.chapter_summaries.clear();
    let slices=vec![slice(0,1,0,3),second];
    assert!(validate_quick_read_coverage(&sources,&slices).unwrap_err().contains("overlap"));
}

#[test]
fn a_missing_chapter_summary_prevents_publication_even_when_text_coverage_is_complete(){
    let sources=vec![source(1,2),source(2,2)];
    let mut second=slice(1,2,0,2);second.chapter_summaries.clear();
    let slices=vec![slice(0,1,0,2),second];
    assert!(validate_quick_read_coverage(&sources,&slices).unwrap_err().contains("summary"));
}

#[test]
fn long_chapter_summaries_from_disjoint_slices_merge_into_one_canonical_chapter_entry(){
    let sources=vec![source(1,6)];
    let mention=|local_id:&str|QuickReadCharacterMention{local_id:local_id.into(),name:"顾岚".into(),aliases:vec![],chapter_idx:1,evidence:"本章出现".into()};
    let mut left=slice(0,1,0,3);left.characters.push(mention("left:hero"));left.chapter_summaries[0].summary="主角进入古城".into();left.chapter_summaries[0].character_refs=vec!["left:hero".into()];left.plot_notes=vec![QuickReadPlotNote{summary:"追查信件".into(),source_chapters:vec![1]}];
    let mut right=slice(1,1,3,6);right.characters.push(mention("right:hero"));right.chapter_summaries[0].summary="主角发现密信".into();right.chapter_summaries[0].character_refs=vec!["right:hero".into()];right.plot_notes=vec![QuickReadPlotNote{summary:"密信指向边城".into(),source_chapters:vec![1]}];
    let slices=vec![left,right];
    assert!(validate_quick_read_coverage(&sources,&slices).is_ok());
    let canonical=canonical_chapter_summaries(&slices);
    assert_eq!(canonical.len(),1);
    assert!(canonical[0].summary.contains("进入古城"));
    assert!(canonical[0].summary.contains("发现密信"));
    assert_eq!(canonical[0].character_refs,vec!["left:hero","right:hero"]);
}

#[test]
fn important_character_mentions_require_valid_original_chapter_provenance(){
    let sources=vec![source(1,4),source(2,4)];
    let mut first=slice(0,1,0,4);
    first.characters.push(QuickReadCharacterMention{local_id:"c_late".into(),name:"晚出场人物".into(),aliases:vec!["另一个名字".into()],chapter_idx:7,evidence:"第七章揭示其身份".into()});
    let second=slice(1,2,0,4);
    assert!(validate_quick_read_coverage(&sources,&[first,second]).unwrap_err().contains("source chapter"));
}

#[test]
fn cross_slice_same_name_is_not_implicitly_merged_without_identity_evidence(){
    let left=QuickReadCharacterMention{local_id:"slice-a:1".into(),name:"沈青".into(),aliases:vec![],chapter_idx:1,evidence:"第一章被称为沈青".into()};
    let right=QuickReadCharacterMention{local_id:"slice-b:1".into(),name:"沈青".into(),aliases:vec![],chapter_idx:40,evidence:"第四十章另一个沈青".into()};
    let cards=merge_character_mentions(&[left.clone(),right.clone()],&[]);
    assert_eq!(cards.len(),2);
    assert_ne!(cards[0].id,cards[1].id);
}

#[test]
fn explicit_identity_evidence_merges_mentions_and_preserves_late_reveal_sources(){
    let left=QuickReadCharacterMention{local_id:"slice-a:1".into(),name:"阿七".into(),aliases:vec![],chapter_idx:2,evidence:"主角收留的侍童".into()};
    let right=QuickReadCharacterMention{local_id:"slice-b:1".into(),name:"顾岚".into(),aliases:vec!["阿七".into()],chapter_idx:40,evidence:"确认顾岚即早年的阿七".into()};
    let cards=merge_character_mentions(&[left,right],&[IdentityEvidence{left_local_id:"slice-a:1".into(),right_local_id:"slice-b:1".into(),reason:"原文明确写出两名相同".into(),source_chapter:40}]);
    assert_eq!(cards.len(),1);
    assert_eq!(cards[0].first_appearance_chapter,2);
    assert!(cards[0].revelations.iter().any(|reveal|reveal.chapter_idx==40));
}

#[test]
fn only_one_character_output_contract_is_used_per_slice(){
    let schema=super::slice_schema();
    let validator=jsonschema::validator_for(&schema).unwrap();
    let valid=json!({"chapter_summaries":[{"chapter_idx":1,"summary":"主角救下侍童","character_refs":["c1"]}],"characters":[{"ref":"c1","name":"阿七","aliases":[],"chapter_idx":1,"evidence":"主角收留的侍童"}],"plot_notes":[]});
    assert!(validator.is_valid(&valid));
    let invalid=json!({"chapter_summaries":[{"chapter_idx":1,"summary":"主角救下侍童","character_refs":["c1"]}],"characters":[{"ref":"c1","name":"阿七","aliases":[],"chapter_idx":1,"evidence":"线索"}],"plot_notes":[],"entity_graph":{"nodes":[],"edges":[]}});
    assert!(!validator.is_valid(&invalid));
}

#[test]
fn canonical_summaries_deduplicate_only_exact_items_not_substrings(){
 let mut first=slice(0,1,0,2);first.chapter_summaries[0].summary="阿七发现密信".into();
 let mut second=slice(1,1,2,4);second.chapter_summaries[0].summary="发现密信".into();
 let mut third=slice(2,1,4,6);third.chapter_summaries[0].summary="阿七发现密信".into();
 assert_eq!(canonical_chapter_summaries(&[first,second,third])[0].summary,"阿七发现密信；发现密信");
}

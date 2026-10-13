use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;
use serde_json::Value;

static SLICE_SCHEMA: LazyLock<Value> = LazyLock::new(|| serde_json::from_str(include_str!("../../schemas/quick-read-slice.schema.json")).expect("quick-read slice schema"));

#[derive(Clone,Debug,Deserialize,Serialize,PartialEq,Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceChapter { pub chapter_idx:i64, pub char_count:usize, pub content_hash:String }

#[derive(Clone,Debug,Deserialize,Serialize,PartialEq,Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceSpan { pub chapter_idx:i64, pub start_char:usize, pub end_char:usize, pub content_hash:String }

#[derive(Clone,Debug,Deserialize,Serialize,PartialEq,Eq)]
#[serde(deny_unknown_fields)]
pub struct ChapterQuickRead { pub chapter_idx:i64, pub summary:String, pub character_refs:Vec<String> }

#[derive(Clone,Debug,Deserialize,Serialize,PartialEq,Eq)]
#[serde(deny_unknown_fields)]
pub struct QuickReadCharacterMention { pub local_id:String, pub name:String, pub aliases:Vec<String>, pub chapter_idx:i64, pub evidence:String }

#[derive(Clone,Debug,Deserialize,Serialize,PartialEq,Eq)]
#[serde(deny_unknown_fields)]
pub struct QuickReadPlotNote { pub summary:String, pub source_chapters:Vec<i64> }

#[derive(Clone,Debug,Deserialize,Serialize,PartialEq,Eq)]
#[serde(deny_unknown_fields)]
pub struct QuickReadSlice {
    pub slice_idx:usize,
    pub source_spans:Vec<SourceSpan>,
    pub chapter_summaries:Vec<ChapterQuickRead>,
    pub characters:Vec<QuickReadCharacterMention>,
    pub plot_notes:Vec<QuickReadPlotNote>,
}

#[derive(Clone,Debug,Deserialize,Serialize,PartialEq,Eq)]
#[serde(deny_unknown_fields)]
pub struct IdentityEvidence { pub left_local_id:String, pub right_local_id:String, pub reason:String, pub source_chapter:i64 }

#[derive(Clone,Debug,Deserialize,Serialize,PartialEq,Eq)]
#[serde(deny_unknown_fields)]
pub struct CharacterReveal { pub chapter_idx:i64, pub evidence:String }

#[derive(Clone,Debug,Deserialize,Serialize,PartialEq,Eq)]
#[serde(deny_unknown_fields)]
pub struct QuickReadCharacterCard {
    pub id:String,
    pub name:String,
    pub aliases:Vec<String>,
    pub identity_evidence:Vec<String>,
    pub motivations:Vec<String>,
    pub narrative_roles:Vec<String>,
    pub first_appearance_chapter:i64,
    pub revelations:Vec<CharacterReveal>,
    pub source_chapters:Vec<i64>,
}

#[derive(Clone,Copy,Debug,Deserialize,Serialize,PartialEq,Eq)]
#[serde(rename_all="snake_case")]
pub enum QuickReadSourceKind { LocalFullText, WebOutline }


#[derive(Clone,Debug,Deserialize,Serialize,PartialEq,Eq)]
#[serde(deny_unknown_fields)]
pub struct QuickReadReference {
    pub schema_version:String,
    pub source_digest:String,
    pub source_kind:QuickReadSourceKind,
    pub provider_id:i64,
    pub model:String,
    pub prompt_version:String,
    pub chapter_summaries:Vec<ChapterQuickRead>,
    pub characters:Vec<QuickReadCharacterCard>,
    pub plot_summary:String,
    pub plot_notes:Vec<QuickReadPlotNote>,
    /// Exact source ranges are available only for a locally read full text.
    pub completed_spans:Vec<SourceSpan>,
    pub source_provenance:Vec<crate::web_search::WebOutlineProvenance>,
}

pub fn slice_schema()->&'static Value {&SLICE_SCHEMA}

/// Every imported chapter must be fully, exactly, and uniquely covered by persisted slices.
pub fn validate_quick_read_coverage(sources:&[SourceChapter],slices:&[QuickReadSlice])->Result<(),String>{
    if sources.is_empty(){return Err("coverage has no source chapters".into());}
    let mut chapters=HashMap::with_capacity(sources.len());
    for source in sources {if source.chapter_idx<1||source.content_hash.is_empty()||chapters.insert(source.chapter_idx,source).is_some(){return Err("coverage has invalid or duplicate source chapter".into());}}
    let mut ordered:HashMap<i64,Vec<(usize,usize)>>=HashMap::with_capacity(sources.len());
    let mut summaries=HashSet::with_capacity(sources.len());
    let mentions:HashSet<_>=slices.iter().flat_map(|slice|slice.characters.iter().map(|mention|mention.local_id.as_str())).collect();
    if mentions.len()!=slices.iter().map(|slice|slice.characters.len()).sum::<usize>(){return Err("coverage contains duplicate local character ref".into());}
    for slice in slices {
        for span in &slice.source_spans {
            let source=chapters.get(&span.chapter_idx).ok_or_else(||format!("coverage references unknown source chapter {}",span.chapter_idx))?;
            if span.content_hash!=source.content_hash||span.start_char>=span.end_char||span.end_char>source.char_count{return Err(format!("coverage has invalid source span for chapter {}",span.chapter_idx));}
            ordered.entry(span.chapter_idx).or_default().push((span.start_char,span.end_char));
        }
        for chapter in &slice.chapter_summaries {
            if !chapters.contains_key(&chapter.chapter_idx)||chapter.summary.trim().is_empty()||!slice.source_spans.iter().any(|span|span.chapter_idx==chapter.chapter_idx){return Err(format!("coverage has invalid summary source in chapter {}",chapter.chapter_idx));}
            if chapter.character_refs.iter().any(|reference|!mentions.contains(reference.as_str())){return Err(format!("coverage summary has undeclared character ref in chapter {}",chapter.chapter_idx));}
            summaries.insert(chapter.chapter_idx);
        }
        for mention in &slice.characters {if mention.local_id.trim().is_empty()||mention.name.trim().is_empty()||mention.evidence.trim().is_empty()||!chapters.contains_key(&mention.chapter_idx)||!slice.source_spans.iter().any(|span|span.chapter_idx==mention.chapter_idx){return Err(format!("coverage has invalid character source chapter {}",mention.chapter_idx));}}
        for note in &slice.plot_notes {if note.summary.trim().is_empty()||note.source_chapters.is_empty()||note.source_chapters.iter().any(|chapter|!chapters.contains_key(chapter)){return Err("coverage has invalid plot source chapters".into());}}
    }
    if summaries.len()!=sources.len(){return Err("coverage is missing a chapter summary".into());}
    for source in sources {
        let mut spans=ordered.remove(&source.chapter_idx).unwrap_or_default();spans.sort_unstable();let mut cursor=0;
        for (start,end) in spans {if start<cursor{return Err(format!("coverage overlap in chapter {}",source.chapter_idx));}if start>cursor{return Err(format!("coverage gap in chapter {}",source.chapter_idx));}cursor=end;}
        if cursor!=source.char_count{return Err(format!("coverage incomplete in chapter {}",source.chapter_idx));}
    }
    Ok(())
}

pub fn canonical_chapter_summaries(slices:&[QuickReadSlice])->Vec<ChapterQuickRead>{
    let mut merged:BTreeMap<i64,(Vec<String>,BTreeSet<String>)>=BTreeMap::new();
    for slice in slices {for chapter in &slice.chapter_summaries {let (summaries,refs)=merged.entry(chapter.chapter_idx).or_default();let summary=chapter.summary.trim();if !summaries.iter().any(|existing|existing==summary){summaries.push(summary.to_owned());}refs.extend(chapter.character_refs.iter().cloned());}}
    merged.into_iter().map(|(chapter_idx,(summaries,refs))|ChapterQuickRead{chapter_idx,summary:summaries.join("；"),character_refs:refs.into_iter().collect()}).collect()
}

/// Merge only explicitly linked cross-slice identities; equal names are not identity evidence.
pub fn merge_character_mentions(mentions:&[QuickReadCharacterMention],evidence:&[IdentityEvidence])->Vec<QuickReadCharacterCard>{
    let mut parent:HashMap<&str,&str>=mentions.iter().map(|mention|(mention.local_id.as_str(),mention.local_id.as_str())).collect();
    fn root<'a>(parent:&HashMap<&'a str,&'a str>,id:&'a str)->&'a str {let mut current=id;while let Some(next)=parent.get(current){if *next==current{break;}current=next;}current}
    for link in evidence {
        if link.reason.trim().is_empty() || link.source_chapter<1 || !parent.contains_key(link.left_local_id.as_str()) || !parent.contains_key(link.right_local_id.as_str()){continue;}
        let left=root(&parent,&link.left_local_id);let right=root(&parent,&link.right_local_id);if left!=right{parent.insert(right,left);}
    }
    let mut groups:BTreeMap<&str,Vec<&QuickReadCharacterMention>>=BTreeMap::new();
    for mention in mentions {let key=root(&parent,&mention.local_id);groups.entry(key).or_default().push(mention);}
    groups.into_values().map(|mut group|{
        group.sort_by_key(|mention|(mention.chapter_idx,mention.local_id.as_str()));let first=group[0];
        let mut aliases=std::collections::BTreeSet::new();let mut sources=std::collections::BTreeSet::new();let mut identity=std::collections::BTreeSet::new();let mut reveals=Vec::new();
        for mention in &group {sources.insert(mention.chapter_idx);identity.insert(mention.evidence.clone());for alias in &mention.aliases{if alias!=&first.name{aliases.insert(alias.clone());}}if mention.chapter_idx!=first.chapter_idx || mention.local_id!=first.local_id{reveals.push(CharacterReveal{chapter_idx:mention.chapter_idx,evidence:mention.evidence.clone()});}}
        for link in evidence.iter().filter(|link|group.iter().any(|mention|mention.local_id==link.left_local_id||mention.local_id==link.right_local_id)){identity.insert(link.reason.clone());sources.insert(link.source_chapter);if link.source_chapter!=first.chapter_idx{reveals.push(CharacterReveal{chapter_idx:link.source_chapter,evidence:link.reason.clone()});}}
        reveals.sort_by_key(|reveal|(reveal.chapter_idx,reveal.evidence.clone()));reveals.dedup();
        QuickReadCharacterCard{id:first.local_id.clone(),name:first.name.clone(),aliases:aliases.into_iter().collect(),identity_evidence:identity.into_iter().collect(),motivations:vec![],narrative_roles:vec![],first_appearance_chapter:first.chapter_idx,revelations:reveals,source_chapters:sources.into_iter().collect()}
    }).collect()
}
#[cfg(test)]
#[path="quick_read_tests.rs"]
mod tests;


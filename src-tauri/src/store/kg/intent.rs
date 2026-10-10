use super::*;

fn edit_row(r:&rusqlite::Row<'_>)->rusqlite::Result<EditRecord>{Ok(EditRecord{id:r.get(0)?,target_type:r.get(1)?,target_id:r.get(2)?,action:r.get(3)?,after_json:r.get(4)?,effective_from:r.get(5)?,active:r.get(6)?})}
pub fn active_edits(conn:&Connection,book_id:i64)->Result<Vec<EditRecord>,String>{
    ensure_book(conn,book_id)?;let mut stmt=conn.prepare("SELECT id,target_type,target_id,action,after_json,effective_from,active FROM edits_log WHERE book_id=?1 AND active=1 ORDER BY id").map_err(err)?;
    let rows=stmt.query_map([book_id],edit_row).map_err(err)?;let mut edits=rows.collect::<Result<Vec<_>,_>>().map_err(err)?;
    for record in &mut edits{
        let kind=match record.target_type.as_str(){"node"=>"n","edge"=>"r","event"=>"e",_=>continue};
        record.target_id=public_key(book_id,kind,&record.target_id);
        if let Some(value)=&record.after_json{let mut data:Value=parse(value)?;data["key"]=Value::String(record.target_id.clone());
            for field in ["src","dst","location"]{if let Some(key)=data.get(field).and_then(Value::as_str){let key=public_key(book_id,"n",key);data[field]=Value::String(key);}}
            if let Some(participants)=data.get("participants").and_then(Value::as_array){let keys=participants.iter().filter_map(Value::as_str).map(|key|public_key(book_id,"n",key)).collect::<Vec<_>>();data["participants"]=serde_json::to_value(keys).map_err(err)?;}
            record.after_json=Some(json(&data)?);
        }
    }Ok(edits)
}
pub(super) fn applicable(conn:&Connection,book:i64,idx:i64)->Result<Vec<EditRecord>,String>{let mut stmt=conn.prepare("SELECT id,target_type,target_id,action,after_json,effective_from,active FROM edits_log WHERE book_id=?1 AND active=1 AND effective_from<=?2 AND (scope='global' OR chapter_idx=?2) ORDER BY id").map_err(err)?;let rows=stmt.query_map(params![book,idx],edit_row).map_err(err)?;let records=rows.collect::<Result<Vec<_>,_>>().map_err(err)?;let mut latest=std::collections::BTreeMap::new();for record in records{latest.insert((record.target_type.clone(),record.target_id.clone()),record);}let mut result=latest.into_values().collect::<Vec<_>>();result.sort_by_key(|r|r.id);Ok(result)}
pub fn intent_constraints(conn:&Connection,book:i64,idx:i64)->Result<Value,String>{
    let mut tombstones=Vec::new();let mut directives=Vec::new();
    for record in applicable(conn,book,idx)? {
        if record.action=="delete" && ["node","edge","event"].contains(&record.target_type.as_str()){
            let (scope,before)=conn.query_row("SELECT scope,before_json FROM edits_log WHERE id=?1 AND book_id=?2",params![record.id,book],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?))).map_err(err)?;
            tombstones.push(serde_json::json!({"kind":record.target_type,"key":record.target_id,"scope":scope,"effective_from":record.effective_from,"data":before.as_deref().map(parse::<Value>).transpose()?}));
        }else if record.target_type=="directive"&&record.action=="upsert"{
            let data:Value=parse(record.after_json.as_deref().ok_or("missing directive intent")?)?;directives.push(serde_json::json!({"polarity":data.get("polarity").and_then(Value::as_str).unwrap_or("positive"),"text":string_field(&data,"text")?}));
        }
    }Ok(serde_json::json!({"tombstones":tombstones,"directives":directives}))
}
pub fn divergence(conn:&Connection,book_id:i64)->Result<Option<i64>,String>{ensure_book(conn,book_id)?;conn.query_row("SELECT min(effective_from) FROM edits_log WHERE book_id=?1 AND active=1",[book_id],|r|r.get(0)).map_err(err)}
pub(super) fn drift_at(conn:&Connection,book:i64,idx:i64)->Result<f64,String>{let records=applicable(conn,book,idx)?;for record in records.iter().rev(){if record.target_type=="drift_pref"{if record.action=="delete"{return Ok(0.0);}let value:Value=parse(record.after_json.as_deref().ok_or("missing drift intent")?)?;return value.get("value").and_then(Value::as_f64).ok_or_else(||"invalid stored drift preference".into());}}Ok(0.0)}
pub fn drift_pref(conn:&Connection,book_id:i64)->Result<f64,String>{drift_at(conn,book_id,progress(conn,book_id)?+1)}
fn invalidate(conn:&Connection,book:i64,from:i64)->Result<(),String>{
    conn.execute("DELETE FROM kg_read_selection WHERE book_id=?1 AND chapter_idx>=?2",params![book,from]).map_err(err)?;
    conn.execute("UPDATE generated_chapters SET active=0 WHERE book_id=?1 AND chapter_idx>=?2",params![book,from]).map_err(err)?;
    conn.execute("UPDATE kg_revisions SET active=0 WHERE text_version_id IN (SELECT id FROM kg_text_versions WHERE book_id=?1 AND chapter_idx>=?2 AND source='generated')",params![book,from]).map_err(err)?;
    conn.execute("UPDATE kg_specs SET active=0 WHERE book_id=?1 AND chapter_idx>=?2",params![book,from]).map_err(err)?;
    conn.execute("UPDATE chapter_graph_meta SET adapted=0,drift_score=NULL,adapted_at=NULL WHERE book_id=?1 AND chapter_idx>=?2",params![book,from]).map_err(err)?;
    conn.execute("UPDATE kg_lines SET version=version+1 WHERE book_id=?1",[book]).map_err(err)?;Ok(())
}
fn string_field(value:&Value,key:&str)->Result<String,String>{value.get(key).and_then(Value::as_str).filter(|s|!s.trim().is_empty()).map(str::to_owned).ok_or_else(||format!("missing or empty {key}"))}
fn canonical_data(conn:&Connection,input:&EditInput,target:&str,idx:i64)->Result<Value,String>{
    if !input.data.is_object(){return Err("edit data must be object".into());}
    if input.action=="delete"{return Ok(serde_json::json!({}));}
    let q=GraphQuery{book_id:input.book_id,scope:"global".into(),purpose:"spec".into(),chapter_idx:Some(idx)};
    let mut existing=view::internal_view(conn,&q)?;
    let revealed=conn.query_row("SELECT revealed_idx=?2 FROM kg_lines WHERE book_id=?1",params![input.book_id,idx],|r|r.get::<_,Option<bool>>(0)).map_err(err)?.unwrap_or(false);
    if !revealed {
        let mut actual=view::internal_view(conn,&GraphQuery{book_id:input.book_id,scope:"global".into(),purpose:"actual".into(),chapter_idx:Some(progress(conn,input.book_id)?)})?;
        for node in existing.nodes.into_iter().filter(|n|n.provenance=="user"){actual.nodes.retain(|n|n.key!=node.key);actual.nodes.push(node);}
        for edge in existing.edges.into_iter().filter(|e|e.provenance=="user"){actual.edges.retain(|e|e.key!=edge.key);actual.edges.push(edge);}
        for event in existing.events.into_iter().filter(|e|e.provenance=="user"){actual.events.retain(|e|e.key!=event.key);actual.events.push(event);}
        existing=actual;
    }
    let mut data=if input.key.is_some(){match input.kind.as_str(){"node"=>existing.nodes.iter().find(|n|n.key==target).map(serde_json::to_value).transpose().map_err(err)?,"edge"=>existing.edges.iter().find(|n|n.key==target).map(serde_json::to_value).transpose().map_err(err)?,"event"=>existing.events.iter().find(|n|n.key==target).map(serde_json::to_value).transpose().map_err(err)?,_=>None}}else{None}.unwrap_or_else(||serde_json::json!({}));
    let allowed:&[&str]=match input.kind.as_str(){"node"=>&["type","name","aliases","summary"],"edge"=>&["src","dst","rel_type","description","status","since_chapter"],"event"=>&["title","order","participants","location","summary","outcome"],"directive"=>&["text","polarity"],"drift_pref"=>&["value"],_=>return Err("invalid edit kind".into())};
    for (key,value) in input.data.as_object().unwrap(){if !allowed.contains(&key.as_str()){return Err(format!("unknown edit field {key}"));}data.as_object_mut().unwrap().insert(key.clone(),value.clone());}
    if ["node","edge","event"].contains(&input.kind.as_str()){data["key"]=Value::String(target.into());data["provenance"]=Value::String("user".into());data["chapter_idx"]=Value::from(idx);}
    match input.kind.as_str(){
        "node"=>{if data.get("type").is_none(){data["type"]=Value::String("character".into());}if data.get("aliases").is_none(){data["aliases"]=serde_json::json!([]);}if data.get("summary").is_none(){data["summary"]=Value::Null;}let node:GraphNode=serde_json::from_value(data.clone()).map_err(err)?;if !ENTITY_TYPES.contains(&node.entity_type.as_str())||node.name.trim().is_empty(){return Err("invalid node type/name".into());}},
        "edge"=>{for field in ["src","dst"]{if let Some(value)=input.data.get(field){let key=value.as_str().ok_or("relation endpoint must be public key")?;data[field]=Value::String(view::authorized_key(conn,input.book_id,"node",key)?);}else if input.key.is_none(){return Err("new relation requires both endpoints".into());}}if data.get("description").is_none(){data["description"]=Value::Null;}if data.get("status").is_none(){data["status"]=Value::String("active".into());}if data.get("since_chapter").is_none(){data["since_chapter"]=Value::from(idx);}let edge:GraphEdge=serde_json::from_value(data.clone()).map_err(err)?;if edge.rel_type.trim().is_empty()||!["active","broken","former","unknown"].contains(&edge.status.as_str()){return Err("invalid relation type/status".into());}},
        "event"=>{if let Some(participants)=input.data.get("participants"){let keys=participants.as_array().ok_or("participants must be array")?.iter().map(|v|view::authorized_key(conn,input.book_id,"node",v.as_str().ok_or("participant must be public key")?)).collect::<Result<Vec<_>,_>>()?;data["participants"]=serde_json::to_value(keys).map_err(err)?;}if let Some(location)=input.data.get("location"){data["location"]=if location.is_null(){Value::Null}else{Value::String(view::authorized_key(conn,input.book_id,"node",location.as_str().ok_or("location must be public key")?)?)};}for field in ["summary","outcome"]{if data.get(field).is_none(){data[field]=Value::String(String::new());}}if data.get("participants").is_none(){data["participants"]=serde_json::json!([]);}if data.get("location").is_none(){data["location"]=Value::Null;}if data.get("order").is_none(){data["order"]=Value::from(0);}let event:GraphEvent=serde_json::from_value(data.clone()).map_err(err)?;if event.title.trim().is_empty()||event.order<0{return Err("invalid event title/order".into());}},
        "directive"=>{string_field(&data,"text")?;if data.get("polarity").is_none(){data["polarity"]=Value::String("positive".into());}if !["positive","negative"].contains(&string_field(&data,"polarity")?.as_str()){return Err("invalid directive polarity".into());}},
        "drift_pref"=>{let value=data.get("value").and_then(Value::as_f64).ok_or("missing drift value")?;if !value.is_finite()||!(0.0..=1.0).contains(&value){return Err("drift preference outside 0..1".into());}},_=>unreachable!()
    }Ok(data)
}
pub fn edit(conn:&Connection,input:&EditInput)->Result<i64,String>{
    ensure_book(conn,input.book_id)?;if !["global","chapter"].contains(&input.scope.as_str())||!["upsert","delete"].contains(&input.action.as_str())||!["node","edge","event","directive","drift_pref"].contains(&input.kind.as_str()){return Err("invalid edit scope/kind/action".into());}
    let idx=progress(conn,input.book_id)?+1;ensure_chapter(conn,input.book_id,idx)?;if idx==1{return Err("first chapter cannot be edited".into());}
    if (input.scope=="chapter" && input.chapter_idx!=Some(idx)) || (input.scope=="global" && input.chapter_idx.is_some()){return Err("only next chapter or global future intent is editable".into());}
    if input.action=="delete" && input.key.is_none(){return Err("delete requires visible target".into());}
    let tx=conn.unchecked_transaction().map_err(err)?;
    let next:i64=tx.query_row("SELECT coalesce(max(id),0)+1 FROM edits_log",[],|r|r.get(0)).map_err(err)?;
    let target=match input.kind.as_str(){"node"|"edge"|"event"=>match &input.key{Some(key)=>view::authorized_key(&tx,input.book_id,&input.kind,key)?,None=>format!("intent-{}-{next}",input.kind)},"drift_pref"=>"drift".into(),"directive"=>{if let Some(key)=&input.key{let found=active_edits(&tx,input.book_id)?.into_iter().find(|r|r.target_type=="directive" && r.target_id==*key).ok_or("unknown directive target")?;found.target_id}else{format!("directive-{next}")}},_=>unreachable!()};
    let data=canonical_data(&tx,input,&target,idx)?;
    let before=if input.action=="delete"&&["node","edge","event"].contains(&input.kind.as_str()){let mut snapshot=input.clone();snapshot.action="upsert".into();Some(json(&canonical_data(&tx,&snapshot,&target,idx)?)?)}else{None};
    tx.execute("INSERT INTO edits_log(book_id,target_type,target_id,action,before_json,after_json,effective_from,scope,chapter_idx,active) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,1)",params![input.book_id,input.kind,target,input.action,before,if input.action=="delete"{None}else{Some(json(&data)?)},idx,input.scope,input.chapter_idx]).map_err(err)?;
    let id=tx.last_insert_rowid();invalidate(&tx,input.book_id,idx)?;tx.commit().map_err(err)?;Ok(id)
}
pub fn revert(conn:&Connection,book_id:i64,edit_id:i64)->Result<(),String>{
    let tx=conn.unchecked_transaction().map_err(err)?;
    let (from,active)=tx.query_row("SELECT effective_from,active FROM edits_log WHERE id=?1 AND book_id=?2",params![edit_id,book_id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,bool>(1)?))).map_err(err)?;
    if !active{return Ok(());} // idempotent, no unnecessary line churn
    tx.execute("UPDATE edits_log SET active=0,reverted_at=datetime('now') WHERE id=?1",[edit_id]).map_err(err)?;
    // Revoke changes the deterministic reading line, not archived immutable prose.
    invalidate(&tx,book_id,from)?;
    tx.commit().map_err(err)
}
pub(super) fn overlay(conn:&Connection,book:i64,idx:i64,view:&mut GraphView)->Result<(),String>{
    for edit in applicable(conn,book,idx)?{
        match edit.target_type.as_str(){
            "node"=>{view.nodes.retain(|n|n.key!=edit.target_id);if edit.action=="upsert"{view.nodes.push(parse(edit.after_json.as_deref().ok_or("missing node intent")?)?);}else{view.edges.retain(|e|e.src!=edit.target_id&&e.dst!=edit.target_id);for e in &mut view.events{e.participants.retain(|k|k!=&edit.target_id);if e.location.as_deref()==Some(&edit.target_id){e.location=None;}}}},
            "edge"=>{view.edges.retain(|e|e.key!=edit.target_id);if edit.action=="upsert"{view.edges.push(parse(edit.after_json.as_deref().ok_or("missing edge intent")?)?);}},
            "event"=>{view.events.retain(|e|e.key!=edit.target_id);if edit.action=="upsert"{view.events.push(parse(edit.after_json.as_deref().ok_or("missing event intent")?)?);}},_=>{}
        }
    }Ok(())
}

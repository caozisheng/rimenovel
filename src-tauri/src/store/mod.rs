// 存储层：连接管理 + 迁移运行器（dev-plan Task 1.1/1.2）
// 迁移文件位于仓库根 migrations/*.sql，编译期内嵌（include_str! 静态清单，
// 避免运行期相对路径在桌面/移动端的不同 cwd 下找不到文件）。

use rusqlite::Connection;
mod kg_migration;
#[cfg(test)]
mod kg_migration_tests;

// 迁移清单：新增迁移时按序追加 (版本号, 文件内容)。
// 版本号 = 文件名前缀，user_version 记录已应用的最新版本。
const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../../../migrations/0001_init.sql")),
    (2, include_str!("../../../migrations/0002_kg_facts.sql")),
    (3, include_str!("../../../migrations/0003_kg_lite.sql")),
    (4, include_str!("../../../migrations/0004_story_wiki.sql")),
    (5, include_str!("../../../migrations/0005_quick_read.sql")),
    (6, include_str!("../../../migrations/0006_story_question_dependency.sql")),
    (7, include_str!("../../../migrations/0007_tavily_provider.sql")),
    (8, include_str!("../../../migrations/0008_web_outline.sql")),
    (9, include_str!("../../../migrations/0009_web_edition_proof.sql")),
];
#[cfg(test)]
mod tavily_migration_tests {
    use super::*;
    #[test]
    fn provider_search_credential_migration_is_idempotent_and_nullable_for_legacy_rows(){
        let conn=Connection::open_in_memory().unwrap();Db::apply_pragmas(&conn).unwrap();Db::new(&conn).migrate().unwrap();
        let columns:i64=conn.query_row("SELECT COUNT(*) FROM pragma_table_info('llm_providers') WHERE name='tavily_key_ref'",[],|row|row.get(0)).unwrap();assert_eq!(columns,1);
        Db::new(&conn).migrate().unwrap();
    }
}

pub struct Db<'a> {
    pub conn: &'a Connection,
}

impl<'a> Db<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// 打开后的基础 PRAGMA：WAL + NORMAL 同步（桌面/移动一致）。
    pub fn apply_pragmas(conn: &Connection) -> rusqlite::Result<()> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    /// 应用未执行的迁移（幂等）。逐条事务，user_version 推进。
    pub fn migrate(&self) -> rusqlite::Result<()> {
        let current: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (version, sql) in MIGRATIONS {
            if *version <= current {
                continue;
            }
            self.conn.execute_batch("BEGIN")?;
            let result=(||->rusqlite::Result<()>{
                if *version==2{kg_migration::before_facts(self.conn)?;}
                self.conn.execute_batch(sql)?;
                if *version==2{kg_migration::after_facts(self.conn)?;}
                if *version==3{kg_migration::migrate(self.conn)?;}
                Ok(())
            })();
            match result {
                Ok(_) => {
                    self.conn.pragma_update(None, "user_version", version)?;
                    self.conn.execute_batch("COMMIT")?;
                }
                Err(e) => {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    return Err(e);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        Db::apply_pragmas(&conn).unwrap();
        conn
    }

    #[test]
    fn migrations_apply_and_version_advances() {
        let conn = mem_db();
        let db = Db::new(&conn);
        db.migrate().unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(v >= 1);
        // 幂等：重复执行不报错、版本不变
        db.migrate().unwrap();
        let v2: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, v2);
    }

}

#[cfg(test)]
mod story_migration_tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn story_tables_migrate_idempotently_and_preserve_old_intent_log() {
        let conn = Connection::open_in_memory().unwrap();
        Db::apply_pragmas(&conn).unwrap();
        Db::new(&conn).migrate().unwrap();
        let tables: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('story_interventions','story_next_wiki_revisions','story_chapter_revisions')", [], |r| r.get(0)).unwrap();
        assert_eq!(tables, 3);
        Db::new(&conn).migrate().unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM edits_log", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }
}

#[cfg(test)]
mod quick_read_migration_tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn quick_read_revision_and_slice_checkpoints_migrate_idempotently_and_cascade_on_book_delete(){
        let conn=Connection::open_in_memory().unwrap();Db::apply_pragmas(&conn).unwrap();Db::new(&conn).migrate().unwrap();
        let book=crate::store::books::create_book_with_chapters(&conn,"QuickRead",None,"txt","test",&[crate::store::books::NewChapter{idx:1,title:"one".into(),text:"original".into(),token_est:2}]).unwrap();
        conn.execute("INSERT INTO quick_read_revisions(book_id,source_digest,dependency_digest,provider_id,model,prompt_version,schema_version,total_chars,total_chapters,total_slices,status) VALUES (?1,'source','deps',1,'reader','prompt','v1',8,1,1,'pending')",[book]).unwrap();
        let revision=conn.last_insert_rowid();
        conn.execute("INSERT INTO quick_read_slices(revision_id,slice_idx,source_hash,start_chapter,start_char,end_chapter,end_char,spans_json,status) VALUES (?1,0,'source',1,0,1,8,'[]','pending')",[revision]).unwrap();
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM quick_read_slices",[],|row|row.get::<_,i64>(0)).unwrap(),1);
        Db::new(&conn).migrate().unwrap();
        crate::store::books::delete_book(&conn,book).unwrap();
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM quick_read_revisions",[],|row|row.get::<_,i64>(0)).unwrap(),0);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM quick_read_slices",[],|row|row.get::<_,i64>(0)).unwrap(),0);
    }

    #[test]
    fn web_source_migration_preserves_ready_local_reference_and_checkpoints() {
        use crate::core::quick_read::*;
        let conn=Connection::open_in_memory().unwrap();Db::apply_pragmas(&conn).unwrap();
        for (version,sql) in MIGRATIONS.iter().filter(|(version,_)|*version<8) {
            if *version==2 {kg_migration::before_facts(&conn).unwrap();}
            conn.execute_batch(sql).unwrap();
            if *version==2 {kg_migration::after_facts(&conn).unwrap();}
            if *version==3 {kg_migration::migrate(&conn).unwrap();}
            conn.pragma_update(None,"user_version",version).unwrap();
        }
        let book=crate::store::books::create_book_with_chapters(&conn,"Legacy",None,"txt","test",&[crate::store::books::NewChapter{idx:1,title:"One".into(),text:"林昭调查".into(),token_est:4}]).unwrap();
        let sources=crate::store::quick_read::source_chapters(&conn,book).unwrap();
        let spans=vec![SourceSpan{chapter_idx:1,start_char:0,end_char:4,content_hash:sources[0].content_hash.clone()}];
        let summary=ChapterQuickRead{chapter_idx:1,summary:"林昭调查钟楼".into(),character_refs:vec!["hero".into()]};
        let slice=QuickReadSlice{slice_idx:0,source_spans:spans.clone(),chapter_summaries:vec![summary.clone()],characters:vec![QuickReadCharacterMention{local_id:"hero".into(),name:"林昭".into(),aliases:vec![],chapter_idx:1,evidence:"林昭调查".into()}],plot_notes:vec![]};
        let reference=QuickReadReference{schema_version:"v1".into(),source_digest:crate::store::quick_read::source_digest(&sources),source_kind:QuickReadSourceKind::LocalFullText,provider_id:1,model:"reader".into(),prompt_version:"v1".into(),chapter_summaries:vec![summary.clone()],characters:vec![QuickReadCharacterCard{id:"hero".into(),name:"林昭".into(),aliases:vec![],identity_evidence:vec!["林昭调查".into()],motivations:vec![],narrative_roles:vec![],first_appearance_chapter:1,revelations:vec![],source_chapters:vec![1]}],plot_summary:"调查钟楼".into(),plot_notes:vec![],completed_spans:spans.clone(),source_provenance:vec![]};
        conn.execute("INSERT INTO quick_read_revisions(book_id,source_digest,dependency_digest,provider_id,model,prompt_version,schema_version,total_chars,total_chapters,completed_chars,completed_slices,total_slices,status,plot_summary,characters_json) VALUES (?1,?2,'deps',1,'reader','v1','v1',4,1,4,1,1,'ready',?3,?4)",rusqlite::params![book,reference.source_digest,reference.plot_summary,serde_json::to_string(&reference.characters).unwrap()]).unwrap();
        let revision=conn.last_insert_rowid();
        conn.execute("INSERT INTO quick_read_slices(revision_id,slice_idx,source_hash,start_chapter,start_char,end_chapter,end_char,spans_json,result_json,status,tokens_in,tokens_out) VALUES (?1,0,'checkpoint',1,0,1,4,?2,?3,'ready',11,7)",rusqlite::params![revision,serde_json::to_string(&spans).unwrap(),serde_json::to_string(&slice).unwrap()]).unwrap();
        conn.execute("INSERT INTO quick_read_chapter_summaries(revision_id,chapter_idx,data_json) VALUES (?1,1,?2)",rusqlite::params![revision,serde_json::to_string(&summary).unwrap()]).unwrap();
        Db::new(&conn).migrate().unwrap();Db::new(&conn).migrate().unwrap();
        assert_eq!(crate::store::quick_read::active_reference(&conn,book).unwrap(),Some(reference));
        assert_eq!(crate::store::quick_read::load_slices(&conn,revision).unwrap(),vec![slice]);
        assert_eq!(conn.query_row("SELECT tokens_in,tokens_out FROM quick_read_slices WHERE revision_id=?1",[revision],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?))).unwrap(),(11,7));
        assert_eq!(conn.query_row("SELECT count(*) FROM pragma_foreign_key_check",[],|row|row.get::<_,i64>(0)).unwrap(),0);
        crate::store::books::delete_book(&conn,book).unwrap();
        assert_eq!(conn.query_row("SELECT count(*) FROM quick_read_slices WHERE revision_id=?1",[revision],|row|row.get::<_,i64>(0)).unwrap(),0);
    }
}

#[cfg(test)]
mod web_edition_proof_migration_tests {
    use super::*;
    use crate::core::quick_read::*;
    use crate::web_search::WebOutlineProvenance;

    fn legacy_ready_database(score:f64)->(Connection,i64,i64,QuickReadReference) {
        let conn=Connection::open_in_memory().unwrap();Db::apply_pragmas(&conn).unwrap();
        for (version,sql) in MIGRATIONS.iter().filter(|(version,_)|*version<9) {
            if *version==2 {kg_migration::before_facts(&conn).unwrap();}
            conn.execute_batch(sql).unwrap();
            if *version==2 {kg_migration::after_facts(&conn).unwrap();}
            if *version==3 {kg_migration::migrate(&conn).unwrap();}
            conn.pragma_update(None,"user_version",version).unwrap();
        }
        let book=books::create_book_with_chapters(&conn,"Legacy web",None,"txt","test",&[
            books::NewChapter{idx:1,title:"One".into(),text:"林昭调查".into(),token_est:4},
            books::NewChapter{idx:2,title:"Two".into(),text:"钟楼谜团".into(),token_est:4},
        ]).unwrap();
        let sources=quick_read::source_chapters(&conn,book).unwrap();
        let reference=QuickReadReference{schema_version:"web-outline/v1".into(),source_digest:quick_read::source_digest(&sources),source_kind:QuickReadSourceKind::WebOutline,provider_id:7,model:"legacy-reader".into(),prompt_version:"web-outline/v1".into(),chapter_summaries:vec![ChapterQuickRead{chapter_idx:1,summary:"调查钟楼".into(),character_refs:vec!["hero".into()]},ChapterQuickRead{chapter_idx:2,summary:"发现谜团".into(),character_refs:vec![]}],characters:vec![QuickReadCharacterCard{id:"hero".into(),name:"林昭".into(),aliases:vec![],identity_evidence:vec!["网页人物佐证".into()],motivations:vec![],narrative_roles:vec![],first_appearance_chapter:1,revelations:vec![],source_chapters:vec![1]}],plot_summary:"调查钟楼谜团".into(),plot_notes:vec![],completed_spans:vec![],source_provenance:vec![WebOutlineProvenance{url:"https://example.com/legacy".into(),title:"Legacy outline".into(),evidence:"公共章节正文佐证".into()}]};
        conn.execute("INSERT INTO quick_read_revisions(book_id,source_digest,dependency_digest,provider_id,model,prompt_version,schema_version,source_kind,total_chars,total_chapters,completed_chars,completed_slices,total_slices,status,active,plot_summary,plot_notes_json,characters_json) VALUES (?1,?2,'legacy-dependency',7,'legacy-reader','web-outline/v1','web-outline/v1','web_outline',0,2,0,0,0,'ready',1,?3,'[]',?4)",rusqlite::params![book,reference.source_digest,reference.plot_summary,serde_json::to_string(&reference.characters).unwrap()]).unwrap();
        let revision=conn.last_insert_rowid();
        conn.execute("INSERT INTO quick_read_web_outlines(revision_id,normalized_book_title,match_score,provenance_json,created_at) VALUES (?1,'Legacy web',?2,?3,'2026-01-02 03:04:05')",rusqlite::params![revision,score,serde_json::to_string(&reference.source_provenance).unwrap()]).unwrap();
        for chapter in &reference.chapter_summaries {
            conn.execute("INSERT INTO quick_read_chapter_summaries(revision_id,chapter_idx,data_json) VALUES (?1,?2,?3)",rusqlite::params![revision,chapter.chapter_idx,serde_json::to_string(chapter).unwrap()]).unwrap();
            conn.execute("INSERT INTO quick_read_web_outline_chapters(revision_id,chapter_idx,outline_heading,source_url) VALUES (?1,?2,?3,'https://example.com/legacy')",rusqlite::params![revision,chapter.chapter_idx,if chapter.chapter_idx==1 {"One"} else {"Two"}]).unwrap();
        }
        (conn,book,revision,reference)
    }

    #[test]
    fn edition_proof_migration_preserves_ready_web_rows_scores_maps_and_keys() {
        for score in [0.82,0.97] {
            let (conn,book,revision,reference)=legacy_ready_database(score);
            Db::new(&conn).migrate().unwrap();Db::new(&conn).migrate().unwrap();
            assert_eq!(conn.query_row("PRAGMA user_version",[],|row|row.get::<_,i64>(0)).unwrap(),9);
            assert_eq!(quick_read::active_reference(&conn,book).unwrap(),Some(reference));
            assert_eq!(conn.query_row("SELECT revision_id,normalized_book_title,match_score,proof_kind,proof_chapter_idx,created_at FROM quick_read_web_outlines WHERE revision_id=?1",[revision],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<f64>>(2)?,row.get::<_,String>(3)?,row.get::<_,i64>(4)?,row.get::<_,String>(5)?))).unwrap(),(revision,"Legacy web".into(),Some(score),"text_similarity".into(),1,"2026-01-02 03:04:05".into()));
            assert_eq!(conn.query_row("SELECT book_id,dependency_digest,provider_id,status,active FROM quick_read_revisions WHERE id=?1",[revision],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?,row.get::<_,String>(3)?,row.get::<_,i64>(4)?))).unwrap(),(book,"legacy-dependency".into(),7,"ready".into(),1));
            assert_eq!(conn.query_row("SELECT count(*) FROM quick_read_web_outline_chapters WHERE revision_id=?1",[revision],|row|row.get::<_,i64>(0)).unwrap(),2);
            assert_eq!(conn.query_row("SELECT count(*) FROM pragma_foreign_key_check",[],|row|row.get::<_,i64>(0)).unwrap(),0);
            books::delete_book(&conn,book).unwrap();
            for table in ["quick_read_web_outlines","quick_read_web_outline_chapters","quick_read_chapter_summaries"] {
                assert_eq!(conn.query_row(&format!("SELECT count(*) FROM {table} WHERE revision_id=?1"),[revision],|row|row.get::<_,i64>(0)).unwrap(),0);
            }
        }
    }

    #[test]
    fn migrated_text_proof_rejects_low_missing_fake_score_and_incomplete_mapping() {
        let (conn,book,revision,_)=legacy_ready_database(0.81);Db::new(&conn).migrate().unwrap();
        assert!(quick_read::active_reference(&conn,book).is_err());
        conn.execute_batch("PRAGMA ignore_check_constraints=ON").unwrap();
        for score in [None,Some(1.01),Some(-0.1)] {
            conn.execute("UPDATE quick_read_web_outlines SET match_score=?2 WHERE revision_id=?1",rusqlite::params![revision,score]).unwrap();
            assert!(quick_read::active_reference(&conn,book).is_err());
        }
        conn.execute("UPDATE quick_read_web_outlines SET match_score=0.9,proof_chapter_idx=2 WHERE revision_id=?1",[revision]).unwrap();assert!(quick_read::active_reference(&conn,book).is_err());
        conn.execute("UPDATE quick_read_web_outlines SET proof_chapter_idx=1 WHERE revision_id=?1",[revision]).unwrap();
        conn.execute("DELETE FROM quick_read_web_outline_chapters WHERE revision_id=?1 AND chapter_idx=2",[revision]).unwrap();
        assert!(quick_read::active_reference(&conn,book).unwrap().is_none());
    }
}

pub mod books;
pub mod story;
pub mod kg;
pub mod quick_read;
pub mod story_projection;

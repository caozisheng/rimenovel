// 存储层：连接管理 + 迁移运行器（dev-plan Task 1.1/1.2）
// 迁移文件位于仓库根 migrations/*.sql，编译期内嵌（include_str! 静态清单，
// 避免运行期相对路径在桌面/移动端的不同 cwd 下找不到文件）。

use rusqlite::Connection;

// 迁移清单：新增迁移时按序追加 (版本号, 文件内容)。
// 版本号 = 文件名前缀，user_version 记录已应用的最新版本。
const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../../../migrations/0001_init.sql"))];

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
            match self.conn.execute_batch(sql) {
                Ok(_) => {
                    self.conn
                        .pragma_update(None, "user_version", version)?;
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
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert!(v >= 1);
        // 幂等：重复执行不报错、版本不变
        db.migrate().unwrap();
        let v2: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, v2);
    }

    #[test]
    fn init_creates_all_13_tables_and_indexes() {
        let conn = mem_db();
        Db::new(&conn).migrate().unwrap();
        let expected = [
            "books", "chapters", "generated_chapters", "edits_log", "reader_intent",
            "directives", "graph_nodes", "graph_edges", "graph_events",
            "chapter_graph_meta", "jobs", "llm_providers", "llm_cache", "llm_usage",
        ];
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let tables: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for t in expected {
            assert!(tables.contains(&t.to_string()), "missing table: {t}");
        }
        let mut stmt = conn
            .prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name LIKE 'idx_%'")
            .unwrap();
        let idx_count: i64 = stmt.query_row([], |r| r.get(0)).unwrap();
        assert!(idx_count >= 9, "expected >=9 custom indexes, got {idx_count}");
    }
}
pub mod books;

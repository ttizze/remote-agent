//! Durable finished-message postings. The canonical item still owns all text.
//! T3's contentless trigram index bounds common-word reads without losing old
//! matches. A missing/unsupported index and two-character CJK use its scan path.
use super::*;
use agent_protocol::search::Query;
use std::collections::{BTreeMap, BTreeSet};

const INDEX: &str = "orchestration_v2_message_search";
const JS_WHITESPACE: &str = "\t\n\u{000b}\u{000c}\r \u{00a0}\u{1680}\u{2000}\u{2001}\u{2002}\u{2003}\u{2004}\u{2005}\u{2006}\u{2007}\u{2008}\u{2009}\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";

// SQL triggers cannot execute the pure Rust matcher. Keep the storage-specific
// eligibility in one expression for insert/update/scans; the independent RPC
// fixtures cover both environments, including Ultrathink/Unicode whitespace.
fn non_placeholder(row: &str) -> String {
    let prompt = agent_protocol::search::ATTACHMENT_ONLY_BOOTSTRAP_PROMPT.replace('\'', "''");
    format!(
        "CASE WHEN lower(substr(json_extract({row}.payload_json,'$.text'),1,11))='ultrathink:' THEN ltrim(substr(json_extract({row}.payload_json,'$.text'),12),'{JS_WHITESPACE}') ELSE json_extract({row}.payload_json,'$.text') END COLLATE BINARY <> '{prompt}'"
    )
}
fn indexed(row: &str) -> String {
    format!("{row}.type IN ('user_message','assistant_message')
        AND coalesce(json_extract({row}.payload_json,'$.streaming'),0)=0
        AND ({row}.type='assistant_message' OR {row}.created_by='user')
        AND NOT EXISTS(SELECT 1 FROM orchestration_v2_projection_threads t WHERE t.thread_id={row}.thread_id AND json_extract(t.payload_json,'$.lineage.relationshipToParent')='subagent')
        AND NOT EXISTS(SELECT 1 FROM orchestration_v2_projection_runs r WHERE r.thread_id={row}.thread_id AND r.run_id={row}.run_id AND r.status='rolled_back')
        AND ({})",non_placeholder(row))
}
fn index_text(row: &str) -> String {
    format!("replace(replace(json_extract({row}.payload_json,'$.text'),'’',''''),'‘','''')")
}

pub(super) fn initialize(connection: &Connection) -> Result<(), String> {
    match connection.execute_batch(&format!("CREATE VIRTUAL TABLE {INDEX} USING fts5(text,content='',contentless_delete=1,tokenize='trigram remove_diacritics 1')")) {
        Ok(())=>{}
        Err(rusqlite::Error::SqliteFailure(code,error)) if code.extended_code==1=>{
            tracing::warn!(target:"agent",operation="storage.message_search",?error,"SQLite message index unavailable; using bounded scans");
            return Ok(());
        }
        Err(error)=>return Err(failure(error)),
    }
    connection.execute_batch(&format!("
        CREATE TRIGGER message_search_insert AFTER INSERT ON orchestration_v2_projection_turn_items
        WHEN {} BEGIN
            INSERT INTO {INDEX}(rowid,text) SELECT new.rowid,{} WHERE json_extract(new.payload_json,'$.text') IS NOT NULL;
        END;
        CREATE TRIGGER message_search_update AFTER UPDATE ON orchestration_v2_projection_turn_items
        WHEN (coalesce(json_extract(old.payload_json,'$.streaming'),0)=0 OR coalesce(json_extract(new.payload_json,'$.streaming'),0)=0)
          AND (old.type IS NOT new.type OR old.created_by IS NOT new.created_by
            OR json_extract(old.payload_json,'$.streaming') IS NOT json_extract(new.payload_json,'$.streaming')
            OR json_extract(old.payload_json,'$.text') IS NOT json_extract(new.payload_json,'$.text'))
        BEGIN
            DELETE FROM {INDEX} WHERE rowid=old.rowid;
            INSERT INTO {INDEX}(rowid,text) SELECT new.rowid,{} WHERE {} AND json_extract(new.payload_json,'$.text') IS NOT NULL;
        END;
        CREATE TRIGGER message_search_delete AFTER DELETE ON orchestration_v2_projection_turn_items
        BEGIN DELETE FROM {INDEX} WHERE rowid=old.rowid; END;
        -- Bex retains rolled-back items for frozen forks and audit. They no
        -- longer belong in the live message index or its candidate budget.
        CREATE TRIGGER message_search_rollback AFTER UPDATE OF status ON orchestration_v2_projection_runs
        WHEN new.status='rolled_back' AND old.status!='rolled_back'
        BEGIN DELETE FROM {INDEX} WHERE rowid IN(SELECT rowid FROM orchestration_v2_projection_turn_items WHERE thread_id=new.thread_id AND run_id=new.run_id); END;
        ",indexed("new"),index_text("new"),index_text("new"),indexed("new"))).map_err(failure)
}

pub(super) struct MessageHit {
    pub quality: f64,
    pub hits: usize,
    pub user: bool,
    pub created_at: String,
}

pub(super) fn matches(
    connection: &Connection,
    query: &Query,
    eligible: &BTreeSet<&str>,
    excluded: &BTreeSet<&str>,
) -> Result<Vec<(String, MessageHit)>, String> {
    let (long, short) = query.message_tokens();
    if long.is_empty() {
        return Ok(Vec::new());
    }
    let has_index: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1 AND type='table')",
            [INDEX],
            |row| row.get(0),
        )
        .map_err(failure)?;
    let use_index = has_index && long.iter().all(|token| token.chars().count() >= 3);
    let search = use_index.then(|| {
        long.iter()
            .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ")
    });
    let mut narrowing: Vec<_> = short
        .iter()
        .copied()
        .filter(|token| token.is_ascii())
        .collect();
    if !use_index {
        narrowing.extend(long);
    }
    let spellings: Vec<_> = short
        .iter()
        .filter(|token| !token.is_ascii())
        .map(|token| agent_protocol::search::short_token_spellings(token))
        .collect();
    let terms = serde_json::to_string(&narrowing).map_err(failure)?;
    let spellings = serde_json::to_string(&spellings).map_err(failure)?;
    let eligible = serde_json::to_string(eligible).map_err(failure)?;
    let excluded = serde_json::to_string(excluded).map_err(failure)?;
    let (from, condition, order) = if use_index {
        (
            format!(
                "{INDEX} JOIN orchestration_v2_projection_turn_items i ON i.rowid={INDEX}.rowid"
            ),
            format!(
                "{INDEX} MATCH ?1 AND {INDEX}.rowid>=coalesce((SELECT rowid FROM {INDEX} WHERE {INDEX} MATCH ?1 ORDER BY rowid DESC LIMIT 1 OFFSET 9999),0)"
            ),
            format!("{INDEX}.rowid"),
        )
    } else {
        ("(SELECT rowid AS search_rowid,* FROM orchestration_v2_projection_turn_items WHERE type IN ('user_message','assistant_message') ORDER BY rowid DESC LIMIT 20000) i".into(),"1".into(),"i.search_rowid".into())
    };
    let sql=format!("SELECT i.thread_id,json_extract(i.payload_json,'$.text'),i.type='user_message',coalesce(json_extract(i.payload_json,'$.startedAt'),i.updated_at)
        FROM {from}
        LEFT JOIN orchestration_v2_projection_runs r ON r.thread_id=i.thread_id AND r.run_id=i.run_id
        WHERE {condition} AND {}
          AND i.thread_id IN(SELECT value FROM json_each(?4))
          AND i.thread_id NOT IN(SELECT value FROM json_each(?5))
          AND NOT(i.input_intent IS 'queued_turn' AND r.status IN ('queued','cancelled'))
          AND coalesce(json_extract(i.payload_json,'$.messageId'),'') NOT LIKE 'async-answer:%'
          AND NOT EXISTS(SELECT 1 FROM json_each(?2) token WHERE instr(lower(json_extract(i.payload_json,'$.text')),token.value)=0)
          AND NOT EXISTS(SELECT 1 FROM json_each(?3) token WHERE NOT EXISTS(SELECT 1 FROM json_each(token.value) spelling WHERE instr(json_extract(i.payload_json,'$.text'),spelling.value)>0))
        ORDER BY {order} DESC LIMIT 2000",indexed("i"));
    let mut statement = connection.prepare(&sql).map_err(failure)?;
    let rows = statement
        .query_map(
            params![search, terms, spellings, eligible, excluded],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(failure)?;
    let mut positions = BTreeMap::<String, usize>::new();
    let mut result: Vec<(String, MessageHit)> = Vec::new();
    for row in rows {
        let (id, text, user, created_at) = row.map_err(failure)?;
        let Some(quality) = query.message(&text) else {
            continue;
        };
        match positions.entry(id.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(result.len());
                result.push((
                    id,
                    MessageHit {
                        quality,
                        hits: 1,
                        user,
                        created_at,
                    },
                ));
            }
            std::collections::btree_map::Entry::Occupied(entry) => {
                let best = &mut result[*entry.get()].1;
                if quality > best.quality {
                    *best = MessageHit {
                        quality,
                        hits: 1,
                        user,
                        created_at,
                    };
                } else if quality == best.quality {
                    best.hits += 1;
                    if user && !best.user || user == best.user && created_at > best.created_at {
                        best.user = user;
                        best.created_at = created_at;
                    }
                }
            }
        }
    }
    Ok(result)
}

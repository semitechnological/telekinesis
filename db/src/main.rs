use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use turso::Builder;

#[derive(serde::Deserialize)]
struct Request {
    id: u64,
    method: String,
    params: serde_json::Value,
}

#[derive(serde::Serialize)]
struct Response {
    id: u64,
    result: Option<serde_json::Value>,
    error: Option<String>,
}

async fn handle_execute(conn: &turso::Connection, id: u64, sql: &str) -> Response {
    match conn.execute(sql, ()).await {
        Ok(changes) => Response {
            id,
            result: Some(serde_json::json!({"rows_affected": changes})),
            error: None,
        },
        Err(e) => Response {
            id,
            result: None,
            error: Some(format!("{e}")),
        },
    }
}

async fn handle_query(conn: &turso::Connection, id: u64, sql: &str) -> Response {
    match conn.query(sql, ()).await {
        Ok(mut rows) => {
            let columns: Vec<String> = rows.column_names().iter().map(|c| c.to_string()).collect();
            let mut results: Vec<serde_json::Value> = Vec::new();
            loop {
                match rows.next().await {
                    Ok(Some(row)) => {
                        let mut row_data: Vec<serde_json::Value> = Vec::new();
                        for i in 0..columns.len() {
                            match row.get_value(i) {
                                Ok(turso::Value::Null) => row_data.push(serde_json::Value::Null),
                                Ok(turso::Value::Integer(n)) => row_data.push(serde_json::json!(n)),
                                Ok(turso::Value::Real(f)) => row_data.push(serde_json::json!(f)),
                                Ok(turso::Value::Text(s)) => row_data.push(serde_json::json!(s)),
                                Ok(turso::Value::Blob(b)) => row_data.push(serde_json::json!(base64_encode(&b))),
                                Err(e) => row_data.push(serde_json::json!(format!("<{e}>"))),
                            }
                        }
                        results.push(serde_json::json!(row_data));
                    }
                    Ok(None) => break,
                    Err(e) => {
                        return Response {
                            id,
                            result: None,
                            error: Some(format!("{e}")),
                        };
                    }
                }
            }

            let cols: Vec<serde_json::Value> = columns.into_iter().map(serde_json::Value::String).collect();
            Response {
                id,
                result: Some(serde_json::json!({"columns": cols, "rows": results})),
                error: None,
            }
        }
        Err(e) => Response {
            id: 0,
            result: None,
            error: Some(format!("{e}")),
        },
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let db_path = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        ":memory:".into()
    };

    let db = Builder::new_local(db_path.to_str().unwrap_or(":memory:"))
        .build()
        .await
        .expect("Failed to open database");
    let conn = db.connect().expect("Failed to connect to database");

    // Schema migration: strip comments, split by semicolons, execute each
    let schema = include_str!("schema.sql");
    let clean_sql: String = schema
        .lines()
        .filter(|l| !l.trim().starts_with("--") && !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    for stmt in clean_sql.split(';') {
        let trimmed = stmt.trim();
        if !trimmed.is_empty() {
            if let Err(e) = conn.execute(trimmed, ()).await {
                eprintln!("Schema migration warning: {e} for: {trimmed}");
            }
        }
    }

    let stdin = io::stdin();
    let stdout = io::stdout();

    for line in stdin.lock().lines() {
        match line {
            Ok(input) => {
                if input.trim().is_empty() {
                    continue;
                }
                let parsed: serde_json::Value = match serde_json::from_str(&input) {
                    Ok(v) => v,
                    Err(e) => {
                        let err_resp = serde_json::json!({
                            "id": null, "result": null,
                            "error": format!("Parse error: {e}"),
                        });
                        let mut out = stdout.lock();
                        let _ = writeln!(out, "{err_resp}");
                        let _ = out.flush();
                        continue;
                    }
                };

                let id = parsed["id"].as_u64().unwrap_or(0);
                let method = parsed["method"].as_str().unwrap_or("").to_string();
                let params = parsed.get("params").cloned().unwrap_or(serde_json::Value::Null);
                let sql = params["sql"].as_str().unwrap_or("");

                let resp = match method.as_str() {
                    "execute" => handle_execute(&conn, id, sql).await,
                    "query" => handle_query(&conn, id, sql).await,
                    "execute_many" => {
                        let sql_arr = params["sql"].as_array().cloned().unwrap_or_default();
                        let mut total = 0u64;
                        let mut err = None;
                        for stmt in &sql_arr {
                            let s = stmt.as_str().unwrap_or("");
                            if s.is_empty() {
                                continue;
                            }
                            match conn.execute(s, ()).await {
                                Ok(c) => total += c as u64,
                                Err(e) => {
                                    err = Some(format!("{e}"));
                                    break;
                                }
                            }
                        }
                        Response {
                            id,
                            result: if err.is_none() {
                                Some(serde_json::json!({"rows_affected": total}))
                            } else {
                                None
                            },
                            error: err,
                        }
                    }
                    "migrate" => {
                        let schema = include_str!("schema.sql");
                        let clean_sql: String = schema
                            .lines()
                            .filter(|l| !l.trim().starts_with("--") && !l.trim().is_empty())
                            .collect::<Vec<_>>()
                            .join("\n");
                        for stmt in clean_sql.split(';') {
                            let trimmed = stmt.trim();
                            if !trimmed.is_empty() {
                                conn.execute(trimmed, ()).await.ok();
                            }
                        }
                        Response {
                            id,
                            result: Some(serde_json::json!({"ok": true})),
                            error: None,
                        }
                    }
                    "close" | "ping" => Response {
                        id,
                        result: Some(serde_json::json!({"pong": true})),
                        error: None,
                    },
                    _ => Response {
                        id,
                        result: None,
                        error: Some(format!("Unknown method: {method}")),
                    },
                };

                let resp_json = serde_json::json!({"id": resp.id, "result": resp.result, "error": resp.error});
                let mut out = stdout.lock();
                let _ = writeln!(out, "{resp_json}");
                let _ = out.flush();
            }
            Err(e) => {
                eprintln!("Stdin error: {e}");
                break;
            }
        }
    }
}

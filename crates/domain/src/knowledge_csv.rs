//! Bounded, lossless UTF-8 comma-separated table ingestion. The first logical
//! record is the header; empty physical lines outside quoted cells are ignored.
//! Locators count logical CSV records, not physical lines in multiline cells.

use crate::{AppError, Chunk, ChunkKind, ChunkLocator, TenantScope, sha256_hex};
use serde_json::json;
use uuid::Uuid;

const MAX_COLUMNS: usize = 256;
const MAX_DATA_ROWS: usize = 10_000;
const MAX_CELL_BYTES: usize = 256 * 1024;
const MAX_CHUNK_TEXT_BYTES: usize = 32 * 1024 * 1024;

/// The csv crate intentionally accepts malformed quotes. This lexical guard
/// enforces RFC-style quoting before delegating all cell decoding to the crate.
/// Bounds are checked before the reader allocates a record's cells.
fn validate_csv(text: &str) -> Result<(), AppError> {
    #[derive(Clone, Copy)]
    enum State {
        Start,
        Bare,
        Quoted,
        Closed,
    }
    let mut state = State::Start;
    let mut columns = 1;
    let mut cell_bytes = 0;
    for byte in text.bytes() {
        state = match (state, byte) {
            (State::Start, b'"') => State::Quoted,
            (State::Bare, b'"') => {
                return Err(AppError::invalid_request(
                    "CSV quote inside an unquoted field",
                ));
            }
            (State::Quoted, b'"') => State::Closed,
            (State::Quoted, _) => State::Quoted,
            (State::Closed, b'"') => State::Quoted,
            (State::Closed, b',' | b'\r' | b'\n')
            | (State::Start | State::Bare, b',' | b'\r' | b'\n') => {
                if byte == b',' {
                    columns += 1;
                    if columns > MAX_COLUMNS {
                        return Err(AppError::invalid_request("CSV exceeds 256 columns"));
                    }
                } else {
                    columns = 1;
                }
                cell_bytes = 0;
                State::Start
            }
            (State::Closed, _) => {
                return Err(AppError::invalid_request(
                    "CSV has text after a closing quote",
                ));
            }
            (State::Start | State::Bare, _) => State::Bare,
        };
        cell_bytes += 1;
        if cell_bytes > MAX_CELL_BYTES {
            return Err(AppError::invalid_request("CSV cell exceeds 256 KiB"));
        }
    }
    if matches!(state, State::Quoted) {
        return Err(AppError::invalid_request(
            "CSV has an unterminated quoted field",
        ));
    }
    Ok(())
}

pub(crate) fn csv_chunks(
    scope: &TenantScope,
    source_version_id: Uuid,
    text: &str,
) -> Result<Vec<Chunk>, AppError> {
    // csv::Reader itself strips one initial BOM. Strip it only for the lexical
    // guard here, keeping a second U+FEFF (if present) as actual header content.
    validate_csv(text.strip_prefix('\u{feff}').unwrap_or(text))?;
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::invalid_request("CSV import requires a project scope"))?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(false)
        .from_reader(text.as_bytes());
    let mut records = reader.records();
    let headers = records
        .next()
        .transpose()
        .map_err(|_| AppError::invalid_request("CSV header is invalid"))?
        .ok_or_else(|| AppError::invalid_request("CSV requires a header and data records"))?;
    let headers = headers.iter().collect::<Vec<_>>();
    let mut chunks = Vec::new();
    let mut text_bytes = 0;
    for record in records {
        let record = record
            .map_err(|_| AppError::invalid_request("CSV records must match the header width"))?;
        if chunks.len() >= MAX_DATA_ROWS {
            return Err(AppError::invalid_request("CSV exceeds 10000 data records"));
        }
        let values = record.iter().collect::<Vec<_>>();
        let text = json!({"headers": headers, "values": values}).to_string();
        text_bytes += text.len();
        if text_bytes > MAX_CHUNK_TEXT_BYTES {
            return Err(AppError::invalid_request("CSV chunk text exceeds 32 MiB"));
        }
        let ordinal = chunks.len() as i32;
        let row = ordinal as u32 + 2;
        chunks.push(Chunk {
            chunk_id: Uuid::new_v4(),
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            source_version_id,
            ordinal,
            kind: ChunkKind::Table,
            text_hash: sha256_hex(text.as_bytes()),
            text,
            locator: ChunkLocator::Csv {
                start_row: row,
                end_row: row,
                start_column: 1,
                end_column: headers.len() as u32,
                header_row: Some(1),
            },
            product_ids: Vec::new(),
            market: None,
            language: None,
            extraction_method: "deterministic_csv_v1".to_owned(),
            confidence: 1.0,
        });
    }
    if chunks.is_empty() {
        return Err(AppError::invalid_request(
            "CSV requires at least one data record",
        ));
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> TenantScope {
        TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        )
    }

    #[test]
    fn csv_preserves_duplicate_empty_headers_and_exact_cells() {
        let chunks = csv_chunks(
            &scope(),
            Uuid::new_v4(),
            "\u{feff}name,,name,formula\r\n\"甲,乙\",\"a\"\"b\",\"line1\r\nline2\",=1+1\r\n001,, 2 kg ,@SUM(A1)\r\n",
        )
        .unwrap();
        assert_eq!(chunks.len(), 2);
        for (index, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.kind, ChunkKind::Table);
            assert_eq!(chunk.text_hash, sha256_hex(chunk.text.as_bytes()));
            assert_eq!(
                chunk.locator,
                ChunkLocator::Csv {
                    start_row: index as u32 + 2,
                    end_row: index as u32 + 2,
                    start_column: 1,
                    end_column: 4,
                    header_row: Some(1),
                }
            );
            let decoded: serde_json::Value = serde_json::from_str(&chunk.text).unwrap();
            assert_eq!(decoded["headers"], json!(["name", "", "name", "formula"]));
        }
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&chunks[0].text).unwrap()["values"],
            json!(["甲,乙", "a\"b", "line1\r\nline2", "=1+1"])
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&chunks[1].text).unwrap()["values"],
            json!(["001", "", " 2 kg ", "@SUM(A1)"])
        );
    }

    #[test]
    fn csv_rejects_malformed_rows_quotes_and_empty_tables() {
        for text in [
            "",
            "\u{feff}",
            "name,value\n",
            "name,value\none,1\ntwo\n",
            "name,value\none,1\ntwo,2,extra\n",
            "name,value\none,\"unfinished",
            "name,value\none,un\"quoted\n",
            "name,value\none,\"closed\"suffix\n",
        ] {
            assert!(csv_chunks(&scope(), Uuid::new_v4(), text).is_err());
        }
    }

    #[test]
    fn csv_limits_amplification_and_record_width() {
        let wide = format!("{}\n{}\n", ",".repeat(MAX_COLUMNS), ",".repeat(MAX_COLUMNS));
        assert!(csv_chunks(&scope(), Uuid::new_v4(), &wide).is_err());
        let oversized = format!("name\n{}\n", "a".repeat(MAX_CELL_BYTES + 1));
        assert!(csv_chunks(&scope(), Uuid::new_v4(), &oversized).is_err());
        let many = format!("name\n{}", "a\n".repeat(MAX_DATA_ROWS + 1));
        assert!(csv_chunks(&scope(), Uuid::new_v4(), &many).is_err());
        let amplified = format!("{}\n{}", "a".repeat(128 * 1024), "b\n".repeat(257));
        assert!(csv_chunks(&scope(), Uuid::new_v4(), &amplified).is_err());
    }

    #[test]
    fn old_csv_locators_still_deserialize() {
        let locator: ChunkLocator = serde_json::from_value(json!({
            "kind": "csv", "start_row": 2, "end_row": 3,
            "start_column": 1, "end_column": 2
        }))
        .unwrap();
        assert!(matches!(
            locator,
            ChunkLocator::Csv {
                header_row: None,
                ..
            }
        ));
    }
}

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
const MAX_GENERATION_SLICE_BYTES: usize = 32 * 1024 * 1024;
const MAX_GENERATION_QUOTE_CHARS: usize = 1600;
const SLICE_METHOD: &str = "deterministic_csv_evidence_v1";

fn append_chunk(
    chunks: &mut Vec<Chunk>,
    scope: &TenantScope,
    source_version_id: Uuid,
    text: String,
    locator: ChunkLocator,
    extraction_method: &str,
    slice_bytes: &mut usize,
) -> Result<(), AppError> {
    if extraction_method == SLICE_METHOD {
        *slice_bytes = slice_bytes
            .checked_add(text.len())
            .filter(|total| *total <= MAX_GENERATION_SLICE_BYTES)
            .ok_or_else(|| AppError::invalid_request("CSV generation slices exceed 32 MiB"))?;
    }
    chunks.push(Chunk {
        chunk_id: Uuid::new_v4(),
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id: scope.project_id.expect("validated project scope"),
        source_version_id,
        ordinal: chunks.len() as i32,
        kind: ChunkKind::Table,
        text_hash: sha256_hex(text.as_bytes()),
        text,
        locator,
        product_ids: Vec::new(),
        market: None,
        language: None,
        extraction_method: extraction_method.to_owned(),
        confidence: 1.0,
    });
    Ok(())
}

fn csv_locator(row: u32, first: u32, last: u32, range: Option<(u32, u32)>) -> ChunkLocator {
    ChunkLocator::Csv {
        start_row: row,
        end_row: row,
        start_column: first,
        end_column: last,
        header_row: Some(1),
        start_char: range.map(|(start, _)| start),
        end_char: range.map(|(_, end)| end),
    }
}

/// Split one oversized cell into contiguous, complete Unicode scalar spans.
/// The original row remains intact; these separately identified quotes only
/// supply bounded generation input. Every span has an exact cell offset.
fn append_cell_spans(
    chunks: &mut Vec<Chunk>,
    scope: &TenantScope,
    source_version_id: Uuid,
    cell_location: (u32, u32),
    cell: &str,
    header: Option<&str>,
    slice_bytes: &mut usize,
) -> Result<(), AppError> {
    let (row, column) = cell_location;
    let mut byte_start = 0;
    let mut char_start = 0_u32;
    while byte_start < cell.len() {
        let remaining = &cell[byte_start..];
        let mut indices = remaining
            .char_indices()
            .take(MAX_GENERATION_QUOTE_CHARS + 1)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if indices.len() <= MAX_GENERATION_QUOTE_CHARS {
            indices.push(remaining.len());
        }
        let mut end_index = indices.len() - 1;
        let text = loop {
            let fragment = &remaining[..indices[end_index]];
            let text = match header {
                Some(header) => json!({"headers":[header],"values":[fragment]}).to_string(),
                None if row == 1 => json!({"headers":[fragment]}).to_string(),
                None => json!({"values":[fragment]}).to_string(),
            };
            if text.chars().count() <= MAX_GENERATION_QUOTE_CHARS {
                break text;
            }
            end_index /= 2;
            assert!(end_index > 0, "even one escaped cell character must fit");
        };
        let char_end = char_start + end_index as u32;
        append_chunk(
            chunks,
            scope,
            source_version_id,
            text,
            csv_locator(row, column, column, Some((char_start, char_end))),
            SLICE_METHOD,
            slice_bytes,
        )?;
        byte_start += indices[end_index];
        char_start = char_end;
    }
    // Preserve even an empty cell in a bounded evidence chunk when its
    // companion header was too large to fit in the same quote.
    if cell.is_empty() {
        let text = if row == 1 {
            json!({"headers":[""]}).to_string()
        } else {
            json!({"values":[""]}).to_string()
        };
        append_chunk(
            chunks,
            scope,
            source_version_id,
            text,
            csv_locator(row, column, column, Some((0, 0))),
            SLICE_METHOD,
            slice_bytes,
        )?;
    }
    Ok(())
}

fn append_generation_slices(
    chunks: &mut Vec<Chunk>,
    scope: &TenantScope,
    source_version_id: Uuid,
    row: u32,
    headers: &[&str],
    values: &[&str],
    slice_bytes: &mut usize,
) -> Result<(), AppError> {
    let mut first_column = 1_u32;
    let mut group_headers: Vec<&str> = Vec::new();
    let mut group_values: Vec<&str> = Vec::new();
    let flush = |chunks: &mut Vec<Chunk>,
                 first_column: u32,
                 group_headers: &mut Vec<&str>,
                 group_values: &mut Vec<&str>,
                 slice_bytes: &mut usize|
     -> Result<(), AppError> {
        if group_headers.is_empty() {
            return Ok(());
        }
        let last_column = first_column + group_headers.len() as u32 - 1;
        let text = json!({"headers":group_headers,"values":group_values}).to_string();
        append_chunk(
            chunks,
            scope,
            source_version_id,
            text,
            csv_locator(row, first_column, last_column, None),
            SLICE_METHOD,
            slice_bytes,
        )?;
        group_headers.clear();
        group_values.clear();
        Ok(())
    };
    for (index, (&header, &value)) in headers.iter().zip(values).enumerate() {
        group_headers.push(header);
        group_values.push(value);
        if json!({"headers":group_headers,"values":group_values})
            .to_string()
            .chars()
            .count()
            <= MAX_GENERATION_QUOTE_CHARS
        {
            continue;
        }
        group_headers.pop();
        group_values.pop();
        flush(
            chunks,
            first_column,
            &mut group_headers,
            &mut group_values,
            slice_bytes,
        )?;
        let column = index as u32 + 1;
        first_column = column;
        let short_header = (header.chars().count() <= 128
            && json!({"headers":[header],"values":["\u{0000}"]})
                .to_string()
                .chars()
                .count()
                <= MAX_GENERATION_QUOTE_CHARS)
            .then_some(header);
        let paired_quote = json!({"headers":[header],"values":[value]}).to_string();
        if paired_quote.chars().count() <= MAX_GENERATION_QUOTE_CHARS {
            group_headers.push(header);
            group_values.push(value);
        } else {
            if short_header.is_none() {
                append_cell_spans(
                    chunks,
                    scope,
                    source_version_id,
                    (1, column),
                    header,
                    None,
                    slice_bytes,
                )?;
            }
            append_cell_spans(
                chunks,
                scope,
                source_version_id,
                (row, column),
                value,
                short_header,
                slice_bytes,
            )?;
            first_column = column + 1;
        }
    }
    flush(
        chunks,
        first_column,
        &mut group_headers,
        &mut group_values,
        slice_bytes,
    )
}

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
    scope
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
    let mut slice_bytes = 0;
    for (data_rows, record) in records.enumerate() {
        let record = record
            .map_err(|_| AppError::invalid_request("CSV records must match the header width"))?;
        if data_rows >= MAX_DATA_ROWS {
            return Err(AppError::invalid_request("CSV exceeds 10000 data records"));
        }
        let values = record.iter().collect::<Vec<_>>();
        let text = json!({"headers": headers, "values": values}).to_string();
        text_bytes += text.len();
        if text_bytes > MAX_CHUNK_TEXT_BYTES {
            return Err(AppError::invalid_request("CSV chunk text exceeds 32 MiB"));
        }
        let row = data_rows as u32 + 2;
        let oversized = text.chars().count() > MAX_GENERATION_QUOTE_CHARS;
        append_chunk(
            &mut chunks,
            scope,
            source_version_id,
            text,
            csv_locator(row, 1, headers.len() as u32, None),
            "deterministic_csv_v1",
            &mut slice_bytes,
        )?;
        if oversized {
            append_generation_slices(
                &mut chunks,
                scope,
                source_version_id,
                row,
                &headers,
                &values,
                &mut slice_bytes,
            )?;
        }
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
                    start_char: None,
                    end_char: None,
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
    fn oversized_records_retain_full_row_and_exact_bounded_slices() {
        let long_header = "列".repeat(1800);
        let long_value = "甲\n\"乙\",".repeat(1100);
        let csv = format!(
            "{long_header},unit,code\n\"{}\", 2 kg ,001\n",
            long_value.replace('"', "\"\"")
        );
        let chunks = csv_chunks(&scope(), Uuid::new_v4(), &csv).unwrap();
        let full = &chunks[0];
        let original: serde_json::Value = serde_json::from_str(&full.text).unwrap();
        assert_eq!(original["headers"][0], long_header);
        assert_eq!(original["values"][0], long_value);
        assert!(full.text.chars().count() > MAX_GENERATION_QUOTE_CHARS);
        assert_eq!(full.extraction_method, "deterministic_csv_v1");
        let mut header_ranges = Vec::new();
        let mut value_ranges = Vec::new();
        for chunk in chunks.iter().skip(1) {
            assert_eq!(chunk.extraction_method, SLICE_METHOD);
            assert!(chunk.text.chars().count() <= MAX_GENERATION_QUOTE_CHARS);
            assert_eq!(chunk.text_hash, sha256_hex(chunk.text.as_bytes()));
            let ChunkLocator::Csv {
                start_row,
                end_row,
                start_column,
                end_column,
                start_char: Some(start),
                end_char: Some(end),
                ..
            } = chunk.locator
            else {
                continue;
            };
            assert_eq!(start_row, end_row);
            assert_eq!((start_column, end_column), (1, 1));
            let payload: serde_json::Value = serde_json::from_str(&chunk.text).unwrap();
            if start_row == 1 {
                header_ranges.push((
                    start,
                    end,
                    payload["headers"][0].as_str().unwrap().to_owned(),
                ));
            } else {
                assert_eq!(start_row, 2);
                value_ranges.push((
                    start,
                    end,
                    payload["values"][0].as_str().unwrap().to_owned(),
                ));
            }
        }
        for (ranges, original) in [(header_ranges, long_header), (value_ranges, long_value)] {
            assert!(ranges.len() > 1);
            let mut previous = 0;
            let mut reassembled = String::new();
            for (start, end, part) in ranges {
                assert_eq!(start, previous);
                assert_eq!(part.chars().count(), (end - start) as usize);
                previous = end;
                reassembled.push_str(&part);
            }
            assert_eq!(reassembled, original);
        }
        assert!(chunks.iter().any(|chunk| chunk.text.contains(" 2 kg ")));
        assert!(chunks.iter().any(|chunk| chunk.text.contains("001")));
    }

    #[test]
    fn supplemental_slice_budget_rejects_before_inserting_an_excess_chunk() {
        let mut chunks = Vec::new();
        let mut bytes = MAX_GENERATION_SLICE_BYTES - 3;
        let result = append_chunk(
            &mut chunks,
            &scope(),
            Uuid::new_v4(),
            "four".to_owned(),
            csv_locator(2, 1, 1, Some((0, 4))),
            SLICE_METHOD,
            &mut bytes,
        );
        assert!(result.is_err());
        assert!(chunks.is_empty());
        assert_eq!(bytes, MAX_GENERATION_SLICE_BYTES - 3);
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

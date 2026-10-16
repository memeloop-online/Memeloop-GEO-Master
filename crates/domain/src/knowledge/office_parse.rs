//! Frozen, format-aware Office parse wire contract. A unit is a bounded DOCX
//! body-element block or an XLSX worksheet row block, never a PDF page.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{AppError, Chunk, ChunkKind, ChunkLocator, TenantScope, sha256_hex};

pub const OFFICE_PARSE_SCHEMA_VERSION: &str = "geo.office.parse.v1";
pub const OFFICE_MAX_UNITS: usize = 20_000;
pub const OFFICE_MAX_UNIT_TEXT_BYTES: usize = 4 * 1024 * 1024;
pub const OFFICE_MAX_DOCUMENT_TEXT_BYTES: usize = 32 * 1024 * 1024;
pub const OFFICE_DOCX_BLOCK_ELEMENTS: u32 = 64;
pub const OFFICE_XLSX_BLOCK_ROWS: u32 = 128;
const OFFICE_CHUNK_CHARS: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OfficeFormat {
    Docx,
    Xlsx,
}

impl OfficeFormat {
    pub fn media_type(self) -> &'static str {
        match self {
            Self::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        }
    }

    pub fn extraction_method(self) -> &'static str {
        match self {
            Self::Docx => "docx-structure-v1",
            Self::Xlsx => "xlsx-cells-v1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OfficeParseCursor {
    pub created_at: DateTime<Utc>,
    pub job_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OfficeParseJobRef {
    pub scope: TenantScope,
    pub job_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OfficeParseLease {
    pub job_id: Uuid,
    pub lease_id: Uuid,
    pub fencing_token: i64,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OfficeDocumentManifest {
    pub schema_version: String,
    pub input_sha256: String,
    pub parser_version: String,
    pub media_type: String,
    pub document: OfficeDocumentStructure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "format", rename_all = "snake_case")]
pub enum OfficeDocumentStructure {
    Docx { units: Vec<DocxUnit> },
    Xlsx { units: Vec<XlsxUnit> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DocxUnit {
    pub unit_id: u32,
    /// Inclusive, zero-based indices into the document's top-level body.
    pub start_body_element: u32,
    pub end_body_element: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct XlsxUnit {
    pub unit_id: u32,
    pub sheet_name: String,
    pub sheet_index: u32,
    /// Inclusive, one-based row bounds for a block of real worksheet rows.
    pub start_row: u32,
    pub end_row: u32,
}

impl OfficeDocumentManifest {
    pub fn format(&self) -> OfficeFormat {
        match self.document {
            OfficeDocumentStructure::Docx { .. } => OfficeFormat::Docx,
            OfficeDocumentStructure::Xlsx { .. } => OfficeFormat::Xlsx,
        }
    }

    pub fn unit_count(&self) -> usize {
        match &self.document {
            OfficeDocumentStructure::Docx { units } => units.len(),
            OfficeDocumentStructure::Xlsx { units } => units.len(),
        }
    }

    pub fn unit_id(&self, ordinal: usize) -> Option<u32> {
        match &self.document {
            OfficeDocumentStructure::Docx { units } => units.get(ordinal).map(|unit| unit.unit_id),
            OfficeDocumentStructure::Xlsx { units } => units.get(ordinal).map(|unit| unit.unit_id),
        }
    }

    pub fn contains_unit(&self, unit_id: u32) -> bool {
        (0..self.unit_count()).any(|ordinal| self.unit_id(ordinal) == Some(unit_id))
    }

    pub fn validate(&self, input_sha256: &str, profile: &str) -> Result<(), AppError> {
        if self.schema_version != OFFICE_PARSE_SCHEMA_VERSION
            || self.input_sha256 != input_sha256
            || self.parser_version != profile
            || self.media_type != self.format().media_type()
            || self.unit_count() == 0
            || self.unit_count() > OFFICE_MAX_UNITS
        {
            return Err(AppError::invalid_request(
                "Office manifest does not match verified input",
            ));
        }
        match &self.document {
            OfficeDocumentStructure::Docx { units } => {
                let mut next = 0;
                for (ordinal, unit) in units.iter().enumerate() {
                    if unit.unit_id as usize != ordinal
                        || unit.start_body_element != next
                        || unit.end_body_element < next
                        || unit.end_body_element - next >= OFFICE_DOCX_BLOCK_ELEMENTS
                    {
                        return Err(AppError::invalid_request("invalid DOCX body-element block"));
                    }
                    next = unit.end_body_element.checked_add(1).ok_or_else(|| {
                        AppError::invalid_request("DOCX body-element index exceeds limit")
                    })?;
                }
            }
            OfficeDocumentStructure::Xlsx { units } => {
                let mut previous: Option<(u32, u32)> = None;
                for (ordinal, unit) in units.iter().enumerate() {
                    if unit.unit_id as usize != ordinal
                        || unit.sheet_name.is_empty()
                        || unit.sheet_name.len() > 256
                        || unit.start_row == 0
                        || unit.end_row < unit.start_row
                        || unit.end_row - unit.start_row >= OFFICE_XLSX_BLOCK_ROWS
                        || previous.is_some_and(|(sheet, row)| {
                            unit.sheet_index < sheet
                                || (unit.sheet_index == sheet && unit.start_row <= row)
                        })
                    {
                        return Err(AppError::invalid_request(
                            "invalid XLSX worksheet row block",
                        ));
                    }
                    previous = Some((unit.sheet_index, unit.end_row));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DocxElement {
    Paragraph {
        body_element_index: u32,
        paragraph_index: u32,
        heading_path: Vec<String>,
        text: String,
    },
    Table {
        body_element_index: u32,
        table_index: u32,
        heading_path: Vec<String>,
        rows: Vec<DocxTableRow>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DocxTableRow {
    pub row_index: u32,
    pub cells: Vec<DocxTableCell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DocxTableCell {
    pub column_index: u32,
    pub text: String,
    pub row_span: u32,
    pub col_span: u32,
    pub merged: bool,
}

impl DocxElement {
    fn body_index(&self) -> u32 {
        match self {
            Self::Paragraph {
                body_element_index, ..
            }
            | Self::Table {
                body_element_index, ..
            } => *body_element_index,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum XlsxCellKind {
    String,
    Number,
    Boolean,
    Date,
    Error,
    FormulaCached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum XlsxCachedValueKind {
    String,
    Number,
    Boolean,
    Date,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct XlsxCell {
    /// A1 coordinate, including the row number, e.g. "B12".
    pub reference: String,
    pub column: u32,
    pub kind: XlsxCellKind,
    /// Raw stored value; never a newly evaluated formula.
    pub value: String,
    /// Optional value as displayed in the workbook's declared number format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_value: Option<String>,
    /// Formula text is evidence only and must never be executed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
    /// None for a formula without a stored cached result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_kind: Option<XlsxCachedValueKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_range: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct XlsxRow {
    pub row: u32,
    pub cells: Vec<XlsxCell>,
    /// Only explicit workbook table metadata can populate this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_range: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OfficeUnitResult {
    DocxSuccess {
        unit_id: u32,
        elements: Vec<DocxElement>,
    },
    XlsxSuccess {
        unit_id: u32,
        rows: Vec<XlsxRow>,
    },
    Failure {
        unit_id: u32,
        code: String,
    },
}

impl OfficeUnitResult {
    pub fn unit_id(&self) -> u32 {
        match self {
            Self::DocxSuccess { unit_id, .. }
            | Self::XlsxSuccess { unit_id, .. }
            | Self::Failure { unit_id, .. } => *unit_id,
        }
    }

    pub fn is_success(&self) -> bool {
        !matches!(self, Self::Failure { .. })
    }

    pub fn validate(&self, manifest: &OfficeDocumentManifest) -> Result<(), AppError> {
        if !manifest.contains_unit(self.unit_id()) {
            return Err(AppError::invalid_request("Office unit is outside manifest"));
        }
        let index = self.unit_id() as usize;
        match (self, &manifest.document) {
            (Self::DocxSuccess { elements, .. }, OfficeDocumentStructure::Docx { units }) => {
                let unit = &units[index];
                if elements.is_empty() {
                    return Err(AppError::invalid_request("empty DOCX unit cannot succeed"));
                }
                let mut last = None;
                let mut size = 0usize;
                for element in elements {
                    let position = element.body_index();
                    if position < unit.start_body_element
                        || position > unit.end_body_element
                        || last.is_some_and(|last| position <= last)
                    {
                        return Err(AppError::invalid_request("DOCX element is outside block"));
                    }
                    last = Some(position);
                    if let DocxElement::Table { rows, .. } = element {
                        if rows.is_empty() {
                            return Err(AppError::invalid_request("DOCX table has no rows"));
                        }
                        let mut last_row = None;
                        for row in rows {
                            if last_row.is_some_and(|previous| row.row_index <= previous) {
                                return Err(AppError::invalid_request(
                                    "DOCX table rows are unordered",
                                ));
                            }
                            last_row = Some(row.row_index);
                            let mut last_column = None;
                            for cell in &row.cells {
                                if cell.row_span == 0
                                    || cell.col_span == 0
                                    || last_column.is_some_and(|column| cell.column_index <= column)
                                {
                                    return Err(AppError::invalid_request(
                                        "invalid DOCX table cell coordinate",
                                    ));
                                }
                                last_column = Some(cell.column_index);
                            }
                        }
                    }
                    size = size.saturating_add(
                        serde_json::to_vec(element)
                            .map_err(|_| {
                                AppError::invalid_request("DOCX element cannot be serialized")
                            })?
                            .len(),
                    );
                }
                if size > OFFICE_MAX_UNIT_TEXT_BYTES {
                    return Err(AppError::invalid_request(
                        "Office unit output exceeds limit",
                    ));
                }
            }
            (Self::XlsxSuccess { rows, .. }, OfficeDocumentStructure::Xlsx { units }) => {
                let unit = &units[index];
                if rows.is_empty() {
                    return Err(AppError::invalid_request("empty XLSX unit cannot succeed"));
                }
                let mut last = None;
                let mut size = 0usize;
                for row in rows {
                    if row.row < unit.start_row
                        || row.row > unit.end_row
                        || last.is_some_and(|last| row.row <= last)
                    {
                        return Err(AppError::invalid_request("XLSX row is outside block"));
                    }
                    last = Some(row.row);
                    let mut last_column = None;
                    if row
                        .header_range
                        .as_ref()
                        .is_some_and(|value| value.is_empty() || value.len() > 128)
                    {
                        return Err(AppError::invalid_request("invalid XLSX header coordinate"));
                    }
                    for cell in &row.cells {
                        if cell.column == 0
                            || last_column.is_some_and(|column| cell.column <= column)
                            || cell.reference != xlsx_cell_reference(cell.column, row.row)
                            || (cell.cached_kind.is_some() != cell.cached_value.is_some())
                            || (matches!(cell.kind, XlsxCellKind::FormulaCached)
                                != cell.formula.is_some())
                            || (!matches!(cell.kind, XlsxCellKind::FormulaCached)
                                && cell.cached_kind.is_some())
                            || (matches!(cell.kind, XlsxCellKind::FormulaCached)
                                && cell.cached_value.is_none()
                                && (!cell.value.is_empty() || cell.display_value.is_some()))
                            || cell.merged_range.as_ref().is_some_and(|range| {
                                range.is_empty() || range.len() > 128 || !range.contains(':')
                            })
                        {
                            return Err(AppError::invalid_request("invalid XLSX cell coordinate"));
                        }
                        last_column = Some(cell.column);
                    }
                    size = size.saturating_add(
                        serde_json::to_vec(row)
                            .map_err(|_| {
                                AppError::invalid_request("XLSX row cannot be serialized")
                            })?
                            .len(),
                    );
                }
                if size > OFFICE_MAX_UNIT_TEXT_BYTES {
                    return Err(AppError::invalid_request(
                        "Office unit output exceeds limit",
                    ));
                }
            }
            (Self::Failure { code, .. }, _) if office_unit_error_code(code) => {}
            _ => {
                return Err(AppError::invalid_request(
                    "Office format and unit do not match",
                ));
            }
        }
        Ok(())
    }
}

pub fn office_unit_error_code(code: &str) -> bool {
    matches!(
        code,
        "parse_failed" | "unit_limit" | "empty_text" | "unsupported_content"
    )
}

pub fn office_document_error_code(code: &str) -> bool {
    matches!(
        code,
        "invalid_docx"
            | "invalid_xlsx"
            | "encrypted_office"
            | "parse_failed"
            | "unit_limit"
            | "unsupported_content"
    )
}

pub fn xlsx_cell_reference(mut column: u32, row: u32) -> String {
    let mut letters = String::new();
    while column > 0 {
        let remainder = (column - 1) % 26;
        letters.insert(0, char::from(b'A' + remainder as u8));
        column = (column - 1) / 26;
    }
    format!("{letters}{row}")
}

fn xlsx_header_covers_column(range: &str, column: u32) -> bool {
    fn column_of(reference: &str) -> Option<u32> {
        let mut column = 0u32;
        let mut length = 0;
        for letter in reference.bytes().take_while(u8::is_ascii_uppercase) {
            column = column
                .checked_mul(26)?
                .checked_add(u32::from(letter - b'A' + 1))?;
            length += 1;
        }
        (length > 0
            && reference[length..]
                .parse::<u32>()
                .ok()
                .is_some_and(|row| row > 0))
        .then_some(column)
    }
    let Some((start, end)) = range.split_once(':') else {
        return false;
    };
    match (column_of(start), column_of(end)) {
        (Some(first), Some(last)) => first <= column && column <= last,
        _ => false,
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfficeParseInput {
    pub bytes: Vec<u8>,
    pub input_sha256: String,
    pub media_type: String,
    pub parser_profile: String,
    pub successful_units: Vec<u32>,
    pub manifest: Option<OfficeDocumentManifest>,
}

impl std::fmt::Debug for OfficeParseInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfficeParseInput")
            .field(
                "bytes",
                &format_args!("<redacted; {} bytes>", self.bytes.len()),
            )
            .field("input_sha256", &self.input_sha256)
            .field("media_type", &self.media_type)
            .field("parser_profile", &self.parser_profile)
            .field("successful_units", &self.successful_units)
            .field("manifest", &self.manifest)
            .finish()
    }
}

pub fn office_unit_chunks(
    scope: &TenantScope,
    source_version_id: Uuid,
    manifest: &OfficeDocumentManifest,
    result: &OfficeUnitResult,
    first_ordinal: i32,
) -> Result<Vec<Chunk>, AppError> {
    result.validate(manifest)?;
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::invalid_request("project_id is required"))?;
    let mut chunks = Vec::new();
    let mut add = |kind: ChunkKind, text: String, locator: ChunkLocator| -> Result<(), AppError> {
        for (offset, part) in text
            .chars()
            .collect::<Vec<_>>()
            .chunks(OFFICE_CHUNK_CHARS)
            .enumerate()
        {
            let piece = part.iter().collect::<String>();
            if piece.trim().is_empty() {
                continue;
            }
            let ordinal = first_ordinal
                .checked_add(
                    i32::try_from(chunks.len())
                        .map_err(|_| AppError::invalid_request("too many Office chunks"))?,
                )
                .ok_or_else(|| AppError::invalid_request("too many Office chunks"))?;
            let offset_chars = u32::try_from(offset * OFFICE_CHUNK_CHARS)
                .map_err(|_| AppError::invalid_request("Office character offset exceeds limit"))?;
            let length = u32::try_from(part.len())
                .map_err(|_| AppError::invalid_request("Office character offset exceeds limit"))?;
            let locator = match &locator {
                ChunkLocator::Docx {
                    heading_path,
                    paragraph_index,
                    body_element_index,
                    table_index,
                    table_row,
                    table_column,
                    table_row_span,
                    table_col_span,
                    table_merged,
                    ..
                } => ChunkLocator::Docx {
                    heading_path: heading_path.clone(),
                    paragraph_index: *paragraph_index,
                    body_element_index: *body_element_index,
                    table_index: *table_index,
                    table_row: *table_row,
                    table_column: *table_column,
                    table_row_span: *table_row_span,
                    table_col_span: *table_col_span,
                    table_merged: *table_merged,
                    start_char: Some(offset_chars),
                    end_char: Some(offset_chars + length),
                },
                ChunkLocator::Xlsx {
                    sheet,
                    range,
                    header_range,
                    cell_kind,
                    display_value,
                    formula,
                    cached_kind,
                    cached_value,
                    merged_range,
                    ..
                } => ChunkLocator::Xlsx {
                    sheet: sheet.clone(),
                    range: range.clone(),
                    header_range: header_range.clone(),
                    cell_kind: cell_kind.clone(),
                    display_value: display_value.clone(),
                    formula: formula.clone(),
                    cached_kind: cached_kind.clone(),
                    cached_value: cached_value.clone(),
                    merged_range: merged_range.clone(),
                    start_char: Some(offset_chars),
                    end_char: Some(offset_chars + length),
                },
                _ => {
                    return Err(AppError::invalid_request(
                        "Office evidence locator required",
                    ));
                }
            };
            chunks.push(Chunk {
                chunk_id: Uuid::new_v4(),
                operator_id: scope.operator_id,
                tenant_id: scope.tenant_id,
                project_id,
                source_version_id,
                ordinal,
                kind,
                text_hash: sha256_hex(piece.as_bytes()),
                text: piece,
                locator,
                product_ids: Vec::new(),
                market: None,
                language: None,
                extraction_method: manifest.format().extraction_method().to_owned(),
                confidence: 1.0,
            });
        }
        Ok(())
    };
    match (result, &manifest.document) {
        (OfficeUnitResult::DocxSuccess { elements, .. }, OfficeDocumentStructure::Docx { .. }) => {
            for element in elements {
                match element {
                    DocxElement::Paragraph {
                        body_element_index,
                        paragraph_index,
                        heading_path,
                        text,
                    } => {
                        add(
                            ChunkKind::Paragraph,
                            text.clone(),
                            ChunkLocator::Docx {
                                heading_path: heading_path.clone(),
                                paragraph_index: *paragraph_index,
                                body_element_index: Some(*body_element_index),
                                table_index: None,
                                table_row: None,
                                table_column: None,
                                table_row_span: None,
                                table_col_span: None,
                                table_merged: None,
                                start_char: None,
                                end_char: None,
                            },
                        )?;
                    }
                    DocxElement::Table {
                        body_element_index,
                        table_index,
                        heading_path,
                        rows,
                    } => {
                        for row in rows {
                            for cell in &row.cells {
                                add(
                                    ChunkKind::Table,
                                    cell.text.clone(),
                                    ChunkLocator::Docx {
                                        heading_path: heading_path.clone(),
                                        paragraph_index: *body_element_index,
                                        body_element_index: Some(*body_element_index),
                                        table_index: Some(*table_index),
                                        table_row: Some(row.row_index),
                                        table_column: Some(cell.column_index),
                                        table_row_span: Some(cell.row_span),
                                        table_col_span: Some(cell.col_span),
                                        table_merged: Some(cell.merged),
                                        start_char: None,
                                        end_char: None,
                                    },
                                )?;
                            }
                        }
                    }
                }
            }
        }
        (
            OfficeUnitResult::XlsxSuccess { unit_id, rows },
            OfficeDocumentStructure::Xlsx { units },
        ) => {
            let unit = &units[*unit_id as usize];
            for row in rows {
                for cell in &row.cells {
                    let evidence_text = if matches!(cell.kind, XlsxCellKind::FormulaCached)
                        && cell.cached_value.is_none()
                    {
                        cell.formula.as_deref().unwrap_or("").to_owned()
                    } else {
                        cell.value.clone()
                    };
                    add(
                        ChunkKind::Table,
                        evidence_text,
                        ChunkLocator::Xlsx {
                            sheet: unit.sheet_name.clone(),
                            range: cell.reference.clone(),
                            header_range: row
                                .header_range
                                .as_ref()
                                .filter(|range| xlsx_header_covers_column(range, cell.column))
                                .cloned(),
                            cell_kind: Some(format!("{:?}", cell.kind).to_ascii_lowercase()),
                            display_value: cell.display_value.clone(),
                            formula: cell.formula.clone(),
                            cached_kind: cell
                                .cached_kind
                                .map(|kind| format!("{kind:?}").to_ascii_lowercase()),
                            cached_value: cell.cached_value.clone(),
                            merged_range: cell.merged_range.clone(),
                            start_char: None,
                            end_char: None,
                        },
                    )?;
                }
            }
        }
        _ => {}
    }
    Ok(chunks)
}

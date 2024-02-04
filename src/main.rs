use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt::Display,
};

use lopdf::{Document, Object, Stream};

#[derive(Debug)]
struct Font {
    encoding: String,
    unicode_map: Option<BTreeMap<u32, u32>>,
}

impl Font {
    fn decode(&self, text: &[u8]) -> String {
        if let Some(unicode_map) = &self.unicode_map {
            // The unicode map uses 16-byte integers, so we have to convert the text to u16.
            let mut result = String::new();
            for byte_pairs in text.chunks_exact(2) {
                let code = u16::from_be_bytes(byte_pairs.try_into().unwrap()) as u32;
                let code = unicode_map.get(&code).unwrap_or(&code);
                result.push(std::char::from_u32(*code).unwrap());
            }
            return result;
        }
        Document::decode_text(Some(self.encoding.as_str()), text)
    }
}

#[derive(PartialEq, Eq, Clone)]
struct TextChunk {
    text: String,
    x: i32,
    y: i32,
}

impl PartialOrd for TextChunk {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TextChunk {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.y.cmp(&other.y).then_with(|| self.x.cmp(&other.x))
    }
}

impl Display for TextChunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.text.fmt(f)
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut document = lopdf::Document::load("test.pdf")?;
    document.decompress();

    let mut fonts = BTreeMap::new();
    // We have to find the fonts for each page, since there is no API to get all of the fonts.
    for page_id in document.get_pages().values() {
        for (font_id, font_data) in document.get_page_fonts(*page_id) {
            if !fonts.contains_key(font_id.as_slice()) {
                let unicode_map =
                    if let Ok(Object::Reference(unicode_map_id)) = font_data.get(b"ToUnicode") {
                        let unicode_map = document
                            .objects
                            .get(unicode_map_id)
                            .expect("Unicode map id invalid");
                        Some(parse_unicode_map(unicode_map.as_stream()?))
                    } else {
                        None
                    };
                let font = Font {
                    encoding: font_data.get_font_encoding().to_owned(),
                    unicode_map,
                };
                fonts.insert(font_id, font);
            }
        }
    }

    let mut text_chunks = Vec::new();

    for (page_number, page_id) in document.get_pages() {
        let mut current_font_id = None;

        let mut in_text = false;
        let mut current_text = String::new();
        let mut x = 0;
        let mut y = 0;

        for operation in document
            .get_and_decode_page_content(page_id)
            .unwrap()
            .operations
        {
            match operation.operator.as_str() {
                "BT" => in_text = true,
                "ET" => {
                    in_text = false;
                    text_chunks.push(TextChunk {
                        text: current_text,
                        x,
                        y,
                    });
                    current_text = String::new();
                }
                "Tf" => {
                    let font_id = operation.operands[0].as_name().unwrap();
                    current_font_id = Some(font_id.to_owned());
                }
                "Tj" if in_text => {
                    let text = operation.operands[0].as_str().unwrap();
                    let font = fonts.get(current_font_id.as_ref().unwrap()).unwrap();
                    current_text.push_str(&font.decode(text));
                }
                "Tm" => {
                    // The matrix is 3x2, where the first two rows give us scaling and stuff, and the third one gives us the position.
                    let new_x = match operation.operands[4] {
                        Object::Integer(x) => x as i32,
                        Object::Real(x) => x as i32,
                        _ => panic!(
                            "Expected integer or real, found {:?}",
                            operation.operands[4]
                        ),
                    };
                    let new_y = match operation.operands[5] {
                        Object::Integer(y) => y as i32,
                        Object::Real(y) => y as i32,
                        _ => panic!(
                            "Expected integer or real, found {:?}",
                            operation.operands[5]
                        ),
                    };
                    x = new_x;
                    y = new_y;
                }
                _ => {}
            }
        }
    }

    let text_chunks = merge_text_rows(&text_chunks);

    // When doing superscripts, the general pattern is that the y position moves upwards rather than downwards.
    // We manipulate this to try to find the superscript offset, which we assume is the most common of these.
    let mut upward_offsets = BTreeMap::new();
    let mut previous_y = 0;
    for text_chunk in text_chunks.iter().skip(1) {
        let offset = text_chunk.y - previous_y;
        // We are only interested in negative offsets, which mean that it moved upwards.
        if offset < 0 {
            *upward_offsets.entry(-offset).or_insert(0) += 1;
        }
        previous_y = text_chunk.y;
    }
    let (&superscript_offset, _) = upward_offsets
        .iter()
        .max_by_key(|(_, &count)| count)
        .expect("No superscript offset");
    println!("Superscript offset: {}", superscript_offset);
    // We assume that if the difference between consecutive chunks is less than or equal to the superscript offset, it is probably a superscript or subscript.
    let mut new_text_chunks = Vec::new();
    let mut last_y = 0;
    let mut last_x = 0;
    for text_chunk in text_chunks {
        if text_chunk.x < last_x {
            // If the x position is less than the last x position, we assume it is a new line.
            last_x = text_chunk.x;
            last_y = text_chunk.y;
            new_text_chunks.push(text_chunk);
            continue;
        }
        let offset = text_chunk.y - last_y;
        if offset.abs() <= superscript_offset && offset != 0 {
            // If the difference is negative, it is a superscript.
            let html_tag_name = if offset > 0 { "sub" } else { "sup" };
            last_x = text_chunk.x;
            new_text_chunks.push(TextChunk {
                text: format!("<{}>{}</{}>", html_tag_name, text_chunk.text, html_tag_name),
                x: text_chunk.x,
                y: last_y,
            });
        } else {
            last_x = text_chunk.x;
            last_y = text_chunk.y;
            new_text_chunks.push(text_chunk);
        }
    }

    let text_chunks = merge_text_rows(&new_text_chunks);

    for text_chunk in text_chunks {
        println!("{}", text_chunk);
    }

    Ok(())
}

fn merge_text_rows(text_chunks: &[TextChunk]) -> Vec<TextChunk> {
    let mut merged_text_chunks = Vec::new();
    let mut last_text_chunk: Option<TextChunk> = None;
    for text_chunk in text_chunks {
        if let Some(last_text_chunk) = last_text_chunk.as_mut() {
            if last_text_chunk.y == text_chunk.y {
                last_text_chunk.text.push_str(&text_chunk.text);
                continue;
            }
            merged_text_chunks.push(last_text_chunk.clone());
        }
        last_text_chunk = Some(text_chunk.clone());
    }
    if let Some(last_text_chunk) = last_text_chunk {
        merged_text_chunks.push(last_text_chunk);
    }
    merged_text_chunks
}

fn parse_unicode_map(unicode_map: &Stream) -> BTreeMap<u32, u32> {
    let operations = unicode_map
        .decode_content()
        .expect("failed to decode unicode map");
    let mut result = BTreeMap::new();
    // The important thing to find is the endbfchar instruction, which has the actual mappings.
    for operation in operations.operations {
        match operation.operator.as_str() {
            "endbfchar" => {
                assert!(
                    operation.operands.len() % 2 == 0,
                    "Expected even number of operands, found {}",
                    operation.operands.len()
                );
                for operands in operation.operands.chunks_exact(2).map(|operands| {
                    operands
                        .into_iter()
                        .map(|operand| {
                            u16::from_be_bytes(
                                operand
                                    .as_str()
                                    .expect(
                                        format!(
                                            "Expected a hexadecimal integer, found {:?}",
                                            operand
                                        )
                                        .as_str(),
                                    )
                                    .try_into()
                                    .expect(
                                        format!(
                                            "Expected a hexadecimal integer, found {:?}",
                                            operand
                                        )
                                        .as_str(),
                                    ),
                            )
                        })
                        .collect::<Vec<_>>()
                }) {
                    let key = operands[0] as u32;
                    let value = operands[1] as u32;
                    result.insert(key, value);
                }
            }
            _ => {}
        }
    }
    result
}

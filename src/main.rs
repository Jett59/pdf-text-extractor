use std::{collections::BTreeMap, error::Error};

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

    let mut current_font_id = None;

    for (page_number, page_id) in document.get_pages() {
        let mut in_text = false;

        let mut x = 0;
        let mut y = 0;

        for operation in document
            .get_and_decode_page_content(page_id)
            .unwrap()
            .operations
        {
            match operation.operator.as_str() {
                "BT" => in_text = true,
                "ET" => in_text = false,
                "Tf" => {
                    let font_id = operation.operands[0].as_name().unwrap();
                    current_font_id = Some(font_id.to_owned());
                }
                "Tj" if in_text => {
                    let text = operation.operands[0].as_str().unwrap();
                    let font = fonts.get(current_font_id.as_ref().unwrap()).unwrap();
                    print!("{}", font.decode(text));
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
                    if new_y > y {
                        println!();
                    }
                    x = new_x;
                    y = new_y;
                }
                //_ if in_text => println!("{:?}", operation.operator.as_str()),
                _ => {}
            }
        }
    }

    Ok(())
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

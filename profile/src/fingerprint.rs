use anyhow::Result;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;

/// A hash of an XML document that doesn't depend on the order of each element's attributes
/// (profiles write them from hash maps, in a different order each time), to compare a profile
/// with the one on disk without keeping a copy.
pub fn xml_fingerprint(xml: &[u8], extra: &[&[u8]]) -> Result<u64> {
    let mut hasher = DefaultHasher::new();
    let mut reader = Reader::from_reader(xml);
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer)? {
            Event::Eof => break,
            Event::Start(element) | Event::Empty(element) => {
                hasher.write(b"<");
                hasher.write(element.name().as_ref().as_bytes());
                let mut attributes = Vec::new();
                for attribute in element.attributes() {
                    let attribute = attribute?;
                    attributes.push((
                        attribute.key.as_ref().as_bytes().to_vec(),
                        attribute.value.as_bytes().to_vec(),
                    ));
                }
                attributes.sort();
                for (key, value) in attributes {
                    hasher.write(&key);
                    hasher.write(b"=");
                    hasher.write(&value);
                }
            }
            Event::End(element) => {
                hasher.write(b"</");
                hasher.write(element.name().as_ref().as_bytes());
            }
            Event::Text(text) => hasher.write(text.as_ref().as_bytes()),
            _ => {}
        }
        buffer.clear();
    }
    for data in extra {
        hasher.write(&(data.len() as u64).to_le_bytes());
        hasher.write(data);
    }
    Ok(hasher.finish())
}

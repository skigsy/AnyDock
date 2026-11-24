use crate::workspace::{file, optional};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Deserializer, de};
use serde_json::value::RawValue;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::Path,
};

pub fn validate(name: &str, pointers: &[String]) -> Result<()> {
    ensure!(!pointers.is_empty(), "{name} has no selected JSON fields");
    let mut seen = BTreeSet::new();
    for pointer in pointers {
        ensure!(
            (pointer.is_empty() || pointer.starts_with('/'))
                && pointer
                    .split('~')
                    .skip(1)
                    .all(|part| part.starts_with(['0', '1'])),
            "{name} has an invalid JSON pointer {pointer:?}"
        );
        ensure!(
            seen.insert(pointer),
            "{name} repeats JSON pointer {pointer:?}"
        );
    }
    Ok(())
}

pub fn read(root: &Path, name: &str, pointers: &[String]) -> Result<Option<Vec<u8>>> {
    let Some(bytes) =
        optional(fs::read(file(root, name)?)).with_context(|| format!("read input {name}"))?
    else {
        return Ok(None);
    };
    let document: &RawValue =
        serde_json::from_slice(&bytes).with_context(|| format!("{name} contains invalid JSON"))?;
    let mut fields = BTreeMap::new();
    for pointer in pointers {
        let value = select(document, pointer)
            .with_context(|| format!("{name} at JSON pointer {pointer:?}"))?;
        fields.insert(pointer, value);
    }
    Ok(Some(serde_json::to_vec_pretty(&fields)?))
}

pub(crate) fn select<'a>(mut value: &'a RawValue, pointer: &str) -> Result<&'a RawValue> {
    if let Some(path) = pointer.strip_prefix('/') {
        for token in path.split('/') {
            let key = token.replace("~1", "/").replace("~0", "~");
            value = match value.get().as_bytes().first() {
                Some(b'{') => serde_json::from_str::<Object<'a>>(value.get())?
                    .0
                    .remove(&key)
                    .with_context(|| format!("missing key {key:?}"))?,
                Some(b'[') => {
                    let index = key
                        .parse::<usize>()
                        .ok()
                        .filter(|index| index.to_string() == key)
                        .with_context(|| format!("invalid array index {key:?}"))?;
                    let items: Vec<&RawValue> = serde_json::from_str(value.get())?;
                    items
                        .get(index)
                        .copied()
                        .with_context(|| format!("missing array index {index}"))?
                }
                _ => bail!("cannot select {key:?} from a scalar value"),
            };
        }
    }
    Ok(value)
}

struct Object<'a>(BTreeMap<String, &'a RawValue>);

impl<'de> Deserialize<'de> for Object<'de> {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;

        impl<'de> de::Visitor<'de> for ObjectVisitor {
            type Value = Object<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object with unique keys")
            }

            fn visit_map<M: de::MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut fields = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, &RawValue>()? {
                    if let std::collections::btree_map::Entry::Vacant(entry) = fields.entry(key) {
                        entry.insert(value);
                    } else {
                        return Err(de::Error::custom(
                            "ambiguous JSON object with a duplicate key",
                        ));
                    }
                }
                Ok(Object(fields))
            }
        }

        decoder.deserialize_map(ObjectVisitor)
    }
}

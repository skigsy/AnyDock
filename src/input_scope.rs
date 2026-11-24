use crate::workspace::{file, optional};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::value::RawValue;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::Path,
};

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Format {
    Exact,
    #[default]
    CanonicalV1,
}

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

pub fn read(
    root: &Path,
    name: &str,
    pointers: &[String],
    format: Format,
) -> Result<Option<Vec<u8>>> {
    let Some(bytes) =
        optional(fs::read(file(root, name)?)).with_context(|| format!("read input {name}"))?
    else {
        return Ok(None);
    };
    let document = parse(&bytes).with_context(|| format!("{name} contains invalid JSON"))?;
    let mut fields = BTreeMap::new();
    for pointer in pointers {
        let value = select(document, pointer)
            .with_context(|| format!("{name} at JSON pointer {pointer:?}"))?;
        let normalized =
            canonical(value).with_context(|| format!("{name} at JSON pointer {pointer:?}"))?;
        fields.insert(
            pointer,
            match format {
                Format::Exact => value.to_owned(),
                Format::CanonicalV1 => normalized,
            },
        );
    }
    Ok(Some(serde_json::to_vec_pretty(&fields)?))
}

pub(crate) fn parse(bytes: &[u8]) -> serde_json::Result<&RawValue> {
    serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes))
}

fn canonical(value: &RawValue) -> serde_json::Result<Box<RawValue>> {
    use serde_json::value::to_raw_value;
    match value.get().as_bytes().first() {
        Some(b'{') => {
            let fields = serde_json::from_str::<Object<'_>>(value.get())?
                .0
                .into_iter()
                .map(|(key, value)| Ok((key, canonical(value)?)))
                .collect::<serde_json::Result<BTreeMap<_, _>>>()?;
            to_raw_value(&fields)
        }
        Some(b'[') => {
            let entries = serde_json::from_str::<Vec<&RawValue>>(value.get())?
                .into_iter()
                .map(canonical)
                .collect::<serde_json::Result<Vec<_>>>()?;
            to_raw_value(&entries)
        }
        Some(b'"') => to_raw_value(&serde_json::from_str::<String>(value.get())?),
        // Keep the source's exact numeric literal instead of rounding through f64.
        _ => Ok(value.to_owned()),
    }
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

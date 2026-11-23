use super::states::{BLOCKS, PROPERTIES, STATES};

/// A block state decoded from its id: the block's name without namespace and its property values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub name: &'static str,
    pub properties: Vec<(&'static str, &'static str)>,
}

impl State {
    /// The value of `key`, if the block has that property.
    pub fn get(&self, key: &str) -> Option<&'static str> {
        self.properties.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }
}

/// Decodes block state `id`; `None` for ids past the last state.
pub fn decode(id: u32) -> Option<State> {
    if id >= STATES {
        return None;
    }
    let at = BLOCKS.partition_point(|(first, _, _)| *first <= id).checked_sub(1)?;
    let (first, name, props) = BLOCKS[at];
    let mut rest = id - first;
    let mut properties = vec![("", ""); props.len()];
    for (slot, &p) in properties.iter_mut().zip(props).rev() {
        let (key, values) = PROPERTIES[usize::from(p)];
        let count = values.len() as u32;
        *slot = (key, values[(rest % count) as usize]);
        rest /= count;
    }
    Some(State { name, properties })
}

/// The id of block `name` with `properties`; a property left out takes its first value.
pub fn encode(name: &str, properties: &[(&str, &str)]) -> Option<u32> {
    let (first, _, props) = *BLOCKS.iter().find(|(_, n, _)| *n == name)?;
    let mut id = 0u32;
    for &p in props {
        let (key, values) = PROPERTIES[usize::from(p)];
        let value = properties.iter().find(|(k, _)| *k == key).map_or(Some(0), |(_, v)| values.iter().position(|x| x == v))?;
        id = id * values.len() as u32 + value as u32;
    }
    Some(first + id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_inverts_decoding() {
        for id in [0, 1, 8, 9, 89, 5000, 20000, STATES - 1] {
            let s = decode(id).unwrap();
            assert_eq!(encode(s.name, &s.properties), Some(id), "{s:?}");
        }
        assert_eq!(encode("stone", &[]), Some(1));
        assert_eq!(encode("no_such_block", &[]), None);
        assert_eq!(encode("grass_block", &[("snowy", "maybe")]), None);
    }

    #[test]
    fn air_stone_and_grass_decode() {
        assert_eq!(decode(0).unwrap().name, "air");
        assert_eq!(decode(1).unwrap().name, "stone");
        let grass = decode(8).unwrap();
        assert_eq!((grass.name, grass.get("snowy")), ("grass_block", Some("true")));
        assert_eq!(decode(9).unwrap().get("snowy"), Some("false"));
        assert!(decode(STATES).is_none());
    }

    #[test]
    fn properties_count_in_sorted_order_with_the_last_fastest() {
        let first = BLOCKS.iter().find(|b| b.1 == "oak_stairs").unwrap().0;
        let s = decode(first + 1).unwrap();
        assert_eq!(s.get("facing"), Some("north"));
        assert_eq!(s.get("half"), Some("top"));
        assert_eq!(s.get("shape"), Some("straight"));
        assert_eq!(s.get("waterlogged"), Some("false"));
        let chest = BLOCKS.iter().find(|b| b.1 == "chest").unwrap().0;
        let c = decode(chest + 2).unwrap();
        assert_eq!((c.get("facing"), c.get("type"), c.get("waterlogged")), (Some("north"), Some("left"), Some("true")));
    }
}

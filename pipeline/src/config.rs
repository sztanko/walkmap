use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct City {
    pub id: String,
    pub name: String,
    pub pbf_url: String,
    /// [west, south, east, north]
    pub bbox: Option<[f64; 4]>,
    #[serde(default = "default_grid_m")]
    pub grid_m: f64,
    #[serde(default)]
    pub pilot: bool,
}

fn default_grid_m() -> f64 {
    10.0
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeatureType {
    pub id: String,
    pub name: String,
    pub r#match: MatchRule,
    /// optional [west, south, east, north] restriction
    pub within: Option<[f64; 4]>,
}

/// Declarative predicate over OSM tags: match(tags, lat/lng) -> bool.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MatchRule {
    Any { any: Vec<MatchRule> },
    All { all: Vec<MatchRule> },
    Not { not: Box<MatchRule> },
    KeyIn { key: String, r#in: Vec<String> },
    KeyExists { key: String, exists: bool },
}

impl MatchRule {
    pub fn matches(&self, tags: &HashMap<&str, &str>) -> bool {
        match self {
            MatchRule::Any { any } => any.iter().any(|r| r.matches(tags)),
            MatchRule::All { all } => all.iter().all(|r| r.matches(tags)),
            MatchRule::Not { not } => !not.matches(tags),
            MatchRule::KeyIn { key, r#in } => tags
                .get(key.as_str())
                .map_or(false, |v| r#in.iter().any(|x| x == v)),
            MatchRule::KeyExists { key, exists } => tags.contains_key(key.as_str()) == *exists,
        }
    }
}

impl FeatureType {
    pub fn matches(&self, tags: &HashMap<&str, &str>, lng: f64, lat: f64) -> bool {
        if let Some([w, s, e, n]) = self.within {
            if lng < w || lng > e || lat < s || lat > n {
                return false;
            }
        }
        self.r#match.matches(tags)
    }
}

pub fn load_cities(config_dir: &Path) -> Result<Vec<City>> {
    let p = config_dir.join("cities.yaml");
    let s = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    Ok(serde_yaml::from_str(&s)?)
}

pub fn load_feature_types(config_dir: &Path) -> Result<Vec<FeatureType>> {
    let p = config_dir.join("feature_types.yaml");
    let s = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    Ok(serde_yaml::from_str(&s)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags<'a>(kv: &[(&'a str, &'a str)]) -> HashMap<&'a str, &'a str> {
        kv.iter().copied().collect()
    }

    #[test]
    fn key_in() {
        let r: MatchRule = serde_yaml::from_str("{ key: amenity, in: [pub, biergarten] }").unwrap();
        assert!(r.matches(&tags(&[("amenity", "pub")])));
        assert!(!r.matches(&tags(&[("amenity", "bar")])));
        assert!(!r.matches(&tags(&[("shop", "pub")])));
    }

    #[test]
    fn nested_all_not() {
        let r: MatchRule = serde_yaml::from_str(
            "all: [{ key: amenity, in: [school] }, { not: { key: school, in: [driving] } }]",
        )
        .unwrap();
        assert!(r.matches(&tags(&[("amenity", "school")])));
        assert!(!r.matches(&tags(&[("amenity", "school"), ("school", "driving")])));
    }

    #[test]
    fn within_bbox() {
        let ft = FeatureType {
            id: "x".into(),
            name: "X".into(),
            r#match: serde_yaml::from_str("{ key: amenity, in: [pub] }").unwrap(),
            within: Some([0.0, 0.0, 1.0, 1.0]),
        };
        assert!(ft.matches(&tags(&[("amenity", "pub")]), 0.5, 0.5));
        assert!(!ft.matches(&tags(&[("amenity", "pub")]), 2.0, 0.5));
    }
}

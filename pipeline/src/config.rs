use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct City {
    pub id: String,
    pub name: String,
    pub pbf_url: String,
    /// ISO-ish country code used to pick feature-type variants (uk, pt, es…)
    #[serde(default)]
    pub country: Option<String>,
    /// feature type ids partitioned for this city
    pub types: Vec<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureType {
    pub id: String,
    pub name: String,
    pub r#match: MatchRule,
    /// optional [west, south, east, north] restriction
    #[serde(default)]
    pub within: Option<[f64; 4]>,
    /// country- or city-keyed overrides of `match` (city id wins over country)
    #[serde(default)]
    pub variants: Option<HashMap<String, Variant>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variant {
    pub r#match: MatchRule,
}

/// Declarative predicate over OSM tags: match(tags, lat/lng) -> bool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MatchRule {
    Any { any: Vec<MatchRule> },
    All { all: Vec<MatchRule> },
    Not { not: Box<MatchRule> },
    KeyIn { key: String, r#in: Vec<String> },
    /// case-insensitive substring match on the tag value
    KeyContains { key: String, contains: Vec<String> },
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
            MatchRule::KeyContains { key, contains } => tags.get(key.as_str()).map_or(false, |v| {
                let v = v.to_lowercase();
                contains.iter().any(|s| v.contains(&s.to_lowercase()))
            }),
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

/// The feature types of one city, with variant `match` rules applied.
/// (Returned types have `variants` stripped — they are fully resolved.)
pub fn resolve_types(city: &City, catalogue: &[FeatureType]) -> Result<Vec<FeatureType>> {
    let mut out = Vec::with_capacity(city.types.len());
    for id in &city.types {
        let base = catalogue
            .iter()
            .find(|t| &t.id == id)
            .with_context(|| format!("city {}: unknown feature type '{id}'", city.id))?;
        let mut t = base.clone();
        if let Some(variants) = t.variants.take() {
            let v = variants
                .get(&city.id)
                .or_else(|| city.country.as_ref().and_then(|c| variants.get(c)));
            if let Some(v) = v {
                t.r#match = v.r#match.clone();
            }
        }
        out.push(t);
    }
    if out.is_empty() {
        bail!("city {} has no feature types configured", city.id);
    }
    Ok(out)
}

/// Stable hash of a city's resolved feature config — keys the extract cache.
pub fn types_hash(types: &[FeatureType]) -> u64 {
    use std::hash::Hasher;
    let json = serde_json::to_string(types).expect("feature types serialize");
    let mut h = rustc_hash::FxHasher::default();
    h.write(json.as_bytes());
    h.finish()
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
    fn key_contains_case_insensitive() {
        let r: MatchRule =
            serde_yaml::from_str("{ key: name, contains: [tesco, \"pingo doce\"] }").unwrap();
        assert!(r.matches(&tags(&[("name", "Tesco Express")])));
        assert!(r.matches(&tags(&[("name", "PINGO DOCE Funchal")])));
        assert!(!r.matches(&tags(&[("name", "Krishna's Supermarket")])));
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

    fn catalogue() -> Vec<FeatureType> {
        serde_yaml::from_str(
            r#"
- id: supermarket
  name: Supermarkets
  match: { all: [ {key: shop, in: [supermarket]}, {key: brand, exists: true} ] }
  variants:
    uk:
      match:
        all:
          - { key: shop, in: [supermarket] }
          - any:
              - { key: brand, exists: true }
              - { key: name, contains: [tesco] }
- id: cafe
  name: Cafés
  match: { key: amenity, in: [cafe] }
"#,
        )
        .unwrap()
    }

    fn city(country: &str, types: &[&str]) -> City {
        City {
            id: "x".into(),
            name: "X".into(),
            pbf_url: "".into(),
            country: Some(country.into()),
            types: types.iter().map(|s| s.to_string()).collect(),
            bbox: None,
            grid_m: 10.0,
            pilot: false,
        }
    }

    #[test]
    fn variant_resolution() {
        let cat = catalogue();
        // default: unbranded Tesco-named shop is NOT a supermarket
        let de = resolve_types(&city("de", &["supermarket"]), &cat).unwrap();
        assert!(!de[0].matches(&tags(&[("shop", "supermarket"), ("name", "Tesco Metro")]), 0.0, 0.0));
        // uk variant: chain name counts even without a brand tag
        let uk = resolve_types(&city("uk", &["supermarket"]), &cat).unwrap();
        assert!(uk[0].matches(&tags(&[("shop", "supermarket"), ("name", "Tesco Metro")]), 0.0, 0.0));
        assert!(!uk[0].matches(&tags(&[("shop", "supermarket"), ("name", "Krishna's")]), 0.0, 0.0));
        // hashes differ between resolved configs
        assert_ne!(types_hash(&de), types_hash(&uk));
    }

    #[test]
    fn unknown_type_errors() {
        assert!(resolve_types(&city("uk", &["nonexistent"]), &catalogue()).is_err());
    }

    #[test]
    fn types_list_selects_subset() {
        let r = resolve_types(&city("pt", &["cafe"]), &catalogue()).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, "cafe");
    }
}

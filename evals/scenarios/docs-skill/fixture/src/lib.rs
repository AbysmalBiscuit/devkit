use serde::Deserialize;

#[derive(Deserialize)]
pub struct Settings {
    pub name: String,
    #[serde(default)]
    pub retries: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { name: "app".into(), retries: 3 }
    }
}

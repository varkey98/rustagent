use std::env;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub service_name: Option<String>,
    pub exporter: Option<Exporter>,
    pub allowed_content_types: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Exporter {
    pub endpoint: Option<String>,
    pub trace_reporter_type: Option<TraceReporterType>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum TraceReporterType {
    Otlp,
    Logging,
}

pub fn load() -> Config {
    let default = match serde_json::to_string(&Config::default()) {
        Ok(cfg) => cfg,
        Err(_) => String::from(""),
    };

    let mut settings = config::Config::builder()
        .add_source(config::File::from_str(&default, config::FileFormat::Json));

    settings = match env::var("AGENT_CONFIG_FILE") {
        Ok(val) => settings.add_source(config::File::with_name(&val)),
        Err(_) => settings,
    };

    let config = settings
        .add_source(config::Environment::with_prefix("AGENT"))
        .build()
        .unwrap();

    match config.try_deserialize() {
        Ok(cfg) => cfg,
        Err(_) => Config::default(),
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            service_name: Some(String::from("rustagent")),
            exporter: Some(Exporter {
                endpoint: Some(String::from("localhost:4317")),
                trace_reporter_type: Some(TraceReporterType::Otlp),
            }),
            allowed_content_types: Some(vec![String::from("json")]),
        }
    }
}

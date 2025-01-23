use std::{fs::File, path::Path, sync::Arc, time::Duration};
use chrono::NaiveDate;
use futures::{stream::FuturesUnordered, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use tokio::{sync::Semaphore, time::sleep};
use tracing::{debug, info, warn, error};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Error)]
enum AppError {
    #[error("I/O: {0}")]
    IoError(#[from] std::io::Error),
    #[error("JSON parse: {0}")]
    JsonError(#[from] serde_json::Error),
    #[error("HTTP: {0}")]
    HttpError(#[from] reqwest::Error),
    #[error("CSV: {0}")]
    CsvError(#[from] csv::Error),
    #[error("General: {0}")]
    General(String),
}

#[derive(Debug, Deserialize)]
struct JeffConfig {
    enable_slack: bool,
    slack_token: String,
    slack_channel_id: String,
    fetch_interval_minutes: u64,
    search_states: Vec<String>,
    date_range: DateRange,
    api_rate_limit_seconds: f64,
    max_concurrent_fetches: usize,
    max_retries: u8,
}

#[derive(Debug, Deserialize)]
struct DateRange {
    start: String,
    end: String,
}

#[derive(Debug, Deserialize)]
struct Location {
    id: usize,
    name: String,
    state: String,
    city: String,
    address: String,
    #[serde(rename = "addressAdditional")]
    address_additional: Option<String>,
    #[serde(rename = "postalCode")]
    postal_code: String,
    #[serde(rename = "phoneNumber")]
    phone_number: Option<String>,
}

/// We capture both the date, our parsed `Location`, and the entire original JSON.
#[derive(Debug)]
struct FetchedLocation {
    date: NaiveDate,
    loc: Location,
    raw_json: String,
}

#[tokio::main]
async fn main() -> Result<(), AppError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = Arc::new(load_config(".jeff")?);
    info!("Loaded config: {:?}", config);

    let client = Client::new();

    if config.fetch_interval_minutes == 0 {
        run_cycle(&client, Arc::clone(&config)).await?;
    } else {
        loop {
            run_cycle(&client, Arc::clone(&config)).await?;
            info!("Sleeping {} minutes...", config.fetch_interval_minutes);
            sleep(Duration::from_secs(config.fetch_interval_minutes * 60)).await;
        }
    }

    Ok(())
}

fn load_config(path: impl AsRef<Path>) -> Result<JeffConfig, AppError> {
    let contents = std::fs::read_to_string(path)?;
    let config: JeffConfig = serde_json::from_str(&contents)?;
    Ok(config)
}

async fn run_cycle(client: &Client, config: Arc<JeffConfig>) -> Result<(), AppError> {
    info!("Starting cycle...");

    let start_date = NaiveDate::parse_from_str(&config.date_range.start, "%Y-%m-%d")
        .map_err(|e| AppError::General(format!("Invalid start date: {e}")))?;

    let end_date = NaiveDate::parse_from_str(&config.date_range.end, "%Y-%m-%d")
        .map_err(|e| AppError::General(format!("Invalid end date: {e}")))?;

    if end_date < start_date {
        return Err(AppError::General("end_date < start_date".to_string()));
    }

    let mut dates = Vec::new();
    let mut current = start_date;
    while current <= end_date {
        dates.push(current);
        current = current.succ_opt().unwrap();
    }

    let semaphore = Arc::new(Semaphore::new(config.max_concurrent_fetches));
    let mut tasks = FuturesUnordered::new();

    for date in dates {
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let c = client.clone();
        let cfg = Arc::clone(&config);

        tasks.push(tokio::spawn(async move {
            let _guard = permit;
            fetch_for_date(&c, cfg, date).await
        }));
    }

    let mut all_locations = Vec::new();
    while let Some(res) = tasks.next().await {
        match res {
            Ok(Ok(fetched)) => {
                all_locations.extend(fetched);
            }
            Ok(Err(e)) => {
                warn!("Error: {e}");
            }
            Err(e) => {
                error!("Task panicked: {e}");
            }
        }
    }

    info!("Fetched {} locations total.", all_locations.len());

    if config.enable_slack {
        let text = build_slack_message(&all_locations);
        if let Err(e) = post_to_slack(client.clone(), &config.slack_token, &config.slack_channel_id, &text).await {
            error!("Error posting Slack: {e}");
        }
    } else {
        if let Err(e) = export_to_csv(&all_locations, "appointments.csv") {
            error!("Error writing CSV: {e}");
        } else {
            info!("Exported data to appointments.csv");
        }
    }

    Ok(())
}

/// Downloads the data for one date and returns all matched locations, each with raw JSON.
async fn fetch_for_date(
    client: &Client,
    config: Arc<JeffConfig>,
    date: NaiveDate,
) -> Result<Vec<FetchedLocation>, AppError> {
    let url = format!(
        "https://ttp.cbp.dhs.gov/schedulerapi/slots/asLocations?minimum=1&filterTimestampBy=on&timestamp={date}&serviceName=Global%20Entry"
    );

    debug!("HTTP GET: {url}");

    let mut attempt = 0;
    let max_retries = config.max_retries;
    let mut last_err: Option<reqwest::Error> = None;
    let mut backoff_secs = 1;

    while attempt < max_retries {
        attempt += 1;
        debug!("Attempt {attempt} of {max_retries} for date {date}");

        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e_net) => {
                warn!("Network error: {e_net}");
                last_err = Some(e_net);
                retry_backoff(attempt, max_retries, &mut backoff_secs, date).await;
                continue;
            }
        };

        debug!("Status code: {}", resp.status());

        // Always read full body as text so we can store entire JSON
        let text_body = match resp.error_for_status() {
            Ok(ok_resp) => ok_resp.text().await?,
            Err(e_status) => {
                warn!("HTTP status error: {e_status}");
                last_err = Some(e_status);
                retry_backoff(attempt, max_retries, &mut backoff_secs, date).await;
                continue;
            }
        };

        debug!("Response body:\n{}", text_body);

        // Parse as an array of arbitrary JSON.
        let data_arr = serde_json::from_str::<Vec<Value>>(&text_body)?;

        // Filter and convert each element
        let mut results = Vec::new();
        for elem in data_arr {
            // Re-serialize each element to keep its "raw" form
            let raw_json = serde_json::to_string(&elem)?;

            // Attempt to parse it into a structured Location
            // If there's a mismatch, skip
            let parsed: Location = match serde_json::from_value(elem) {
                Ok(loc) => loc,
                Err(e) => {
                    warn!("Failed to parse location: {e}");
                    continue;
                }
            };

            // Filter by states
            if config.search_states.contains(&parsed.state) {
                results.push(FetchedLocation {
                    date,
                    loc: parsed,
                    raw_json,
                });
            }
        }

        sleep(Duration::from_secs_f64(config.api_rate_limit_seconds)).await;
        return Ok(results);
    }

    if let Some(e) = last_err {
        Err(AppError::HttpError(e))
    } else {
        Err(AppError::General(format!("Unknown error fetching date {date}")))
    }
}

async fn retry_backoff(
    attempt: u8,
    max: u8,
    backoff_secs: &mut u64,
    date: NaiveDate,
) {
    if attempt < max {
        warn!("Retrying date {date} in {backoff_secs} second(s)...");
        sleep(Duration::from_secs(*backoff_secs)).await;
        *backoff_secs *= 2;
    }
}

fn build_slack_message(fetched_locations: &[FetchedLocation]) -> String {
    if fetched_locations.is_empty() {
        return "No Global Entry appointments found.".to_string();
    }

    let mut msg = String::new();
    msg.push_str("*Global Entry Availability*\n\n");
    for (i, item) in fetched_locations.iter().enumerate().take(5) {
        let loc = &item.loc;
        let extra = loc.address_additional.as_deref().unwrap_or("");
        let phone = loc.phone_number.as_deref().unwrap_or("N/A");
        msg.push_str(&format!(
            "{}. (Date: {}) *{}* (ID: {}) in {}, {}\nAddress: {} {}\nZip: {}\nPhone: {}\n\n",
            i + 1,
            item.date,
            loc.name,
            loc.id,
            loc.city,
            loc.state,
            loc.address,
            extra,
            loc.postal_code,
            phone
        ));
    }

    if fetched_locations.len() > 5 {
        msg.push_str(&format!("...and {} more.\n", fetched_locations.len() - 5));
    }
    msg
}

async fn post_to_slack(
    client: Client,
    token: &str,
    channel: &str,
    text: &str,
) -> Result<(), AppError> {
    let url = "https://slack.com/api/chat.postMessage";
    debug!("Slack POST: {url}, channel={channel}");

    let payload = serde_json::json!({
        "channel": channel,
        "text": text
    });

    let resp = client
        .post(url)
        .bearer_auth(token)
        .json(&payload)
        .send()
        .await?
        .error_for_status()?;

    #[derive(Deserialize)]
    struct SlackResp {
        ok: bool,
        error: Option<String>,
    }

    let sr: SlackResp = resp.json().await?;
    if !sr.ok {
        Err(AppError::General(sr.error.unwrap_or("Slack unknown error".to_string())))
    } else {
        Ok(())
    }
}

/// Write CSV including the entire raw JSON for each location.
fn export_to_csv(fetched_locations: &[FetchedLocation], path: &str) -> Result<(), AppError> {
    let file = File::create(path)?;
    let mut wtr = csv::Writer::from_writer(file);

    // We now include a column for "RawJSON"
    wtr.write_record(&[
        "Date", 
        "ID", 
        "Name", 
        "State", 
        "City", 
        "Address", 
        "PostalCode", 
        "Phone", 
        "RawJSON"
    ])?;

    for item in fetched_locations {
        let loc = &item.loc;
        let phone = loc.phone_number.as_deref().unwrap_or("N/A");
        wtr.write_record(&[
            item.date.to_string(),
            loc.id.to_string(),
            loc.name.to_string(),
            loc.state.to_string(),
            loc.city.to_string(),
            loc.address.to_string(),
            loc.postal_code.to_string(),
            phone.to_string(),
            item.raw_json.to_string(),
        ])?;
    }

    wtr.flush()?;
    Ok(())
}

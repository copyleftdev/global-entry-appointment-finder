# Global Entry Appointment Finder

This tool queries the TTP CBP system for Global Entry interview appointments within a specified date range, filters them by state, and then **exports the data** in CSV by default or **posts summaries to Slack** if enabled in `.jeff`. It can run once or repeatedly at a defined interval, preserving the full JSON data of each location if desired.

## Features

- Fetches appointments for multiple dates in **parallel**.
- Configurable **states**, date range, concurrency, and retry logic via a `.jeff` file.
- **Rate-limits** requests to avoid hammering the API.
- **Default CSV output** includes:
  - Date of the appointment search
  - Basic location data (ID, name, phone, etc.)
  - **Full raw JSON** of each location object for complete details.
- Optional **Slack integration** (via `enable_slack` in `.jeff`):
  - Posts a message summarizing appointment locations.
- **One-shot** or **continuous** operation based on `fetch_interval_minutes`.

## Requirements

- [Docker](https://docs.docker.com/get-docker/) and [Docker Compose](https://docs.docker.com/compose/).
- **Optional** Slack credentials (token and channel ID) if `enable_slack` is `true`.
- A `.jeff` file at the project root (see example below).

## Configuration

Create a `.jeff` file at the project root. For example:

```json
{
  "enable_slack": false,
  "slack_token": "xoxb-...",
  "slack_channel_id": "C01234567",
  "fetch_interval_minutes": 0,
  "search_states": [
    "AL","AK","AZ","AR","CA","CO","CT","DE","FL","GA",
    "HI","ID","IL","IN","IA","KS","KY","LA","ME","MD",
    "MA","MI","MN","MS","MO","MT","NE","NV","NH","NJ",
    "NM","NY","NC","ND","OH","OK","OR","PA","RI","SC",
    "SD","TN","TX","UT","VT","VA","WA","WV","WI","WY"
  ],
  "date_range": {
    "start": "2025-01-01",
    "end": "2025-02-23"
  },
  "api_rate_limit_seconds": 1.0,
  "max_concurrent_fetches": 5,
  "max_retries": 3
}
```

### Fields

- **`enable_slack`**: `false` uses CSV output; `true` sends a Slack message instead.  
- **`slack_token`** & **`slack_channel_id`**: Used only if `enable_slack` is true.  
- **`fetch_interval_minutes`**: If `0`, run once; otherwise loop indefinitely, sleeping for this many minutes.  
- **`search_states`**: List of 2-letter state codes to include.  
- **`date_range`**: A start/end (`YYYY-MM-DD`) for which days to fetch.  
- **`api_rate_limit_seconds`**: Delay between each date fetch to avoid hitting rate limits.  
- **`max_concurrent_fetches`**: Max parallel requests.  
- **`max_retries`**: Retries per date on network errors or non-200 responses.

## Usage

1. **Build and run**:
   ```bash
   make build
   make run
   ```
2. **Logs**:
   ```bash
   make logs
   ```
3. **Stop**:
   ```bash
   make stop
   ```

### Default CSV Output

When `enable_slack = false`, the tool writes `appointments.csv` with columns like:

- **Date** (the date queried)  
- **ID, Name, State, City, Address, PostalCode, Phone**  
- **RawJSON** (entire source object from the TTP API)

This captures **all** details from each location returned.

### Slack Integration

If `enable_slack = true`, the application instead posts a summary to the specified Slack channel, showing a subset of fields for the first few locations. The CSV output step is skipped.

## Docker Compose Workflow

- **Multi-stage build** for a smaller final image.
- Mounts `.jeff` read-only, so local edits reflect inside the container.
- The container restarts unless stopped, continuing to fetch at your interval (`fetch_interval_minutes`) if > 0.

## Mermaid Flow

```mermaid
flowchart TB
    A[Start Application] --> B[Load .jeff Config]
    B --> C[Initialize Reqwest Client and Slack Token]
    C --> D[Check fetch_interval_minutes]
    D -- "0" --> E[Run Once, Fetch Appointments]
    D -- "Non-Zero" --> F[Loop Forever]
    E --> G[Fetch for Each Date in Range]
    F --> G
    G --> H[Filter by States]
    H --> I[Convert to CSV Rows / Slack Data]
    I --> J{enable_slack?}
    J -- "No" --> K[Write to CSV: includes full JSON]
    J -- "Yes" --> L[Post Slack Summary]
    K --> M[End or Sleep]
    L --> M[End or Sleep]
    M -- "Loop" --> N[Wait fetch_interval_minutes]
    N --> G
    M -- "Once" --> O[Stop]
```

- **A → B → C → D**: App reads `.jeff` and sets up HTTP/Slack.  
- **D → E or F**: If `0`, run once; otherwise loop.  
- **G → H**: For each date, fetch TTP data, filter states.  
- **H → I**: Convert results to CSV rows or Slack messages.  
- **J**: If Slack is enabled, post Slack; otherwise export CSV with full JSON data.  
- **M**: End if one-shot, or **N** if looping.  

You now have **full control** over whether you capture the **entire source JSON** in CSV or post a **Slack** message with essential details— **all configurable** via `.jeff`.
use serde::{Deserialize, Serialize};

// =============================================================================
// Mapped / cleaned output types (used for JSON export)
// =============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct MappedPassenger {
    pub serial: String,
    pub booking_status: String,
    pub current_status: String,
    pub coach_position: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JourneyInfo {
    pub train_number: String,
    pub train_name: String,
    pub boarding_date: String,
    pub from: String,
    pub to: String,
    pub reserved_upto: String,
    pub boarding_point: String,
    pub class: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FareInfo {
    pub total_fare: String,
    pub charting_status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MappedResponse {
    pub pnr: String,
    pub journey: JourneyInfo,
    pub passengers: Vec<MappedPassenger>,
    pub fare: FareInfo,
}

#[derive(Debug, Clone, Serialize)]
pub struct PnrResult {
    pub success: bool,
    pub error: Option<String>,
    pub raw: Option<serde_json::Value>,
    pub mapped: Option<MappedResponse>,
    pub prediction: Option<serde_json::Value>,
    pub elapsed: f64,
}

// =============================================================================
// Disk session cache
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCache {
    pub cookies: Vec<CookieEntry>,
    pub ts: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookieEntry {
    pub name: String,
    pub value: String,
}

// =============================================================================
// UA disk cache
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UaCache {
    pub ua: String,
    pub ts: f64,
}

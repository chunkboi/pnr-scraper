use serde::{Deserialize, Serialize};
use std::borrow::Cow;

// =============================================================================
// Mapped / cleaned output types (used for JSON export)
// =============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct MappedPassenger {
    pub serial: String,
    pub booking_status: String,
    pub current_status: String,
    pub coach_position: String,
    pub quota: String,
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
    pub generated_at: String,
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

// =============================================================================
// Zero-Allocation / Borrowed API models
// =============================================================================

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawApiResponse<'a> {
    pub flag: Option<&'a str>,
    #[serde(borrow)]
    pub pnr_number: Option<MaybeNumber<'a>>,
    #[serde(borrow)]
    pub train_number: Option<MaybeNumber<'a>>,
    pub train_name: Option<&'a str>,
    pub date_of_journey: Option<&'a str>,
    pub source_station: Option<&'a str>,
    pub destination_station: Option<&'a str>,
    pub reservation_upto: Option<&'a str>,
    pub boarding_point: Option<&'a str>,
    pub journey_class: Option<&'a str>,
    pub chart_status: Option<&'a str>,
    pub error_message: Option<&'a str>,
    #[serde(borrow)]
    pub passenger_list: Option<Vec<RawPassenger<'a>>>,
    pub information_message: Option<Vec<&'a str>>,
    pub generated_time_stamp: Option<RawTimeStamp>,
    // Fare fields (checked in sequence by ui::extract_fare)
    #[serde(borrow)]
    pub total_fare: Option<MaybeNumber<'a>>,
    #[serde(borrow)]
    pub ticket_fare: Option<MaybeNumber<'a>>,
    #[serde(borrow)]
    pub booking_fare: Option<MaybeNumber<'a>>,
    #[serde(borrow)]
    pub fare: Option<MaybeNumber<'a>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawPassenger<'a> {
    #[serde(borrow)]
    pub passenger_serial_number: Option<MaybeNumber<'a>>,
    pub booking_status: Option<&'a str>,
    #[serde(borrow)]
    pub booking_berth_no: Option<MaybeNumber<'a>>,
    pub booking_status_details: Option<&'a str>,
    pub current_status: Option<&'a str>,
    #[serde(borrow)]
    pub current_berth_no: Option<MaybeNumber<'a>>,
    pub current_status_details: Option<&'a str>,
    #[serde(borrow)]
    pub current_coach_id: Option<MaybeNumber<'a>>,
    pub passenger_quota: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
pub struct RawTimeStamp {
    pub day: u32,
    pub month: u32,
    pub year: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum MaybeNumber<'a> {
    Str(&'a str),
    Num(u64),
}

impl<'a> MaybeNumber<'a> {
    pub fn as_cow(&self) -> Cow<'a, str> {
        match self {
            MaybeNumber::Str(s) => Cow::Borrowed(s),
            MaybeNumber::Num(n) => Cow::Owned(n.to_string()),
        }
    }
}

use gtk::glib;
use serde_json::{Map, Value, json};

use super::client::Client;
use super::error::Result;

#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    /// Unix seconds.
    pub time: f64,
    pub state: String,
    /// Present only when fetched with attributes.
    pub attributes: Map<String, Value>,
}

/// States of one entity over the last `hours`, oldest first. The first point is
/// the state at the start of the period. Attributes multiply the response size,
/// so they are requested only for charts drawn from them.
pub async fn fetch(
    client: &Client,
    entity_id: &str,
    hours: i64,
    with_attributes: bool,
) -> Result<Vec<Point>> {
    let now = glib::DateTime::now_utc().map_err(|_| super::Error::Protocol("clock".into()))?;
    let start = now
        .add_hours(-(hours as i32))
        .ok()
        .and_then(|start| start.format_iso8601().ok())
        .ok_or_else(|| super::Error::Protocol("clock".into()))?;
    let result = client
        .call(json!({
            "type": "history/history_during_period",
            "start_time": start.as_str(),
            "entity_ids": [entity_id],
            "minimal_response": !with_attributes,
            "no_attributes": !with_attributes,
            "significant_changes_only": false,
        }))
        .await?;
    Ok(parse(&result, entity_id))
}

/// Points use compressed keys: `s` state, `a` attributes, `lu` last updated,
/// `lc` last changed. `lc` is omitted when it equals `lu`, and attribute-only
/// updates keep the old `lc`, so `lu` is the time of each row.
fn parse(result: &Value, entity_id: &str) -> Vec<Point> {
    result[entity_id]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|point| {
            let time = point["lu"].as_f64().or_else(|| point["lc"].as_f64())?;
            Some(Point {
                time,
                state: point["s"].as_str()?.to_owned(),
                attributes: point["a"].as_object().cloned().unwrap_or_default(),
            })
        })
        .collect()
}

/// Numeric points only; unavailable or text states are dropped.
pub fn numeric(points: &[Point]) -> Vec<(f64, f64)> {
    points
        .iter()
        .filter_map(|point| Some((point.time, point.state.parse::<f64>().ok()?)))
        .filter(|(_, value)| value.is_finite())
        .collect()
}

/// Values of one numeric attribute with repeats dropped. Rows where it is
/// missing or null (a thermostat that is off) become NaN, which breaks the line.
pub fn attribute(points: &[Point], key: &str) -> Vec<(f64, f64)> {
    let mut values: Vec<(f64, f64)> = Vec::new();
    for point in points {
        let value = point
            .attributes
            .get(key)
            .and_then(Value::as_f64)
            .unwrap_or(f64::NAN);
        let repeat = values
            .last()
            .is_some_and(|(_, last)| *last == value || (last.is_nan() && value.is_nan()));
        if !repeat {
            values.push((point.time, value));
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_response() {
        let result = json!({
            "sensor.temp": [
                { "s": "20.0", "lu": 100.0 },
                { "s": "unavailable", "lu": 150.0 },
                { "s": "21.5", "lu": 200.0, "lc": 190.0 }
            ]
        });
        let points = parse(&result, "sensor.temp");
        assert_eq!(points.len(), 3);
        assert_eq!(points[2].time, 200.0);
        assert_eq!(numeric(&points), vec![(100.0, 20.0), (200.0, 21.5)]);
        assert!(parse(&result, "sensor.other").is_empty());
    }

    #[test]
    fn attribute_series() {
        let result = json!({
            "climate.x": [
                { "s": "heat", "a": { "current_temperature": 20.0, "temperature": 21.0 }, "lu": 100.0 },
                { "s": "heat", "a": { "current_temperature": 20.5, "temperature": 21.0 }, "lu": 150.0, "lc": 100.0 },
                { "s": "off", "a": { "current_temperature": 20.5, "temperature": null }, "lu": 200.0 }
            ]
        });
        let points = parse(&result, "climate.x");
        assert_eq!(
            attribute(&points, "current_temperature"),
            vec![(100.0, 20.0), (150.0, 20.5)]
        );
        let target = attribute(&points, "temperature");
        assert_eq!(target[0], (100.0, 21.0));
        assert!(target[1].0 == 200.0 && target[1].1.is_nan());
    }
}

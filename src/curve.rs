//! Fan curves and the fan controller state machine. Pure logic, no NVML.
//!
//! Compared to a lookup table rebuilt from the points, the curve is
//! interpolated on demand (a handful of points, so this is cheaper than
//! keeping a table in sync) and is defined over every temperature: below the
//! first point it holds the first speed, above the last point the last speed.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// One curve point: temperature in °C and fan speed in percent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub temp: f32,
    pub speed: f32,
}

impl Point {
    pub const fn new(temp: f32, speed: f32) -> Self {
        Self { temp, speed }
    }
}

/// A fan curve: points sorted by temperature.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Vec<Point>", into = "Vec<Point>")]
pub struct FanCurve {
    points: Vec<Point>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CurveError {
    #[error("a fan curve needs at least one point")]
    Empty,
    #[error("temperature {0} °C is out of range (0–120)")]
    Temp(f32),
    #[error("speed {0} % is out of range (0–100)")]
    Speed(f32),
    #[error("two points have the same temperature ({0} °C)")]
    Duplicate(f32),
}

impl FanCurve {
    /// Builds a curve, sorting the points. Rejects empty curves, values out
    /// of range and two points at the same temperature.
    pub fn new(mut points: Vec<Point>) -> Result<Self, CurveError> {
        if points.is_empty() {
            return Err(CurveError::Empty);
        }
        for p in &points {
            if !(0.0..=120.0).contains(&p.temp) {
                return Err(CurveError::Temp(p.temp));
            }
            if !(0.0..=100.0).contains(&p.speed) {
                return Err(CurveError::Speed(p.speed));
            }
        }
        points.sort_by(|a, b| a.temp.total_cmp(&b.temp));
        if let Some(w) = points.windows(2).find(|w| w[0].temp == w[1].temp) {
            return Err(CurveError::Duplicate(w[0].temp));
        }
        Ok(Self { points })
    }

    pub fn points(&self) -> &[Point] {
        &self.points
    }

    /// Fan speed (percent) at `temp` °C, linearly interpolated.
    pub fn speed_at(&self, temp: f32) -> f32 {
        let pts = &self.points;
        let first = pts[0];
        let last = pts[pts.len() - 1];
        if temp <= first.temp {
            return first.speed;
        }
        if temp >= last.temp {
            return last.speed;
        }
        // Few points: a linear scan beats a binary search here.
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            if temp <= b.temp {
                let t = (temp - a.temp) / (b.temp - a.temp);
                return a.speed + t * (b.speed - a.speed);
            }
        }
        last.speed
    }

    /// A quiet default: silent at idle, full speed before the slowdown point.
    pub fn default_curve() -> Self {
        Self {
            points: vec![
                Point::new(40.0, 30.0),
                Point::new(55.0, 40.0),
                Point::new(65.0, 55.0),
                Point::new(75.0, 75.0),
                Point::new(83.0, 100.0),
            ],
        }
    }
}

impl Default for FanCurve {
    fn default() -> Self {
        Self::default_curve()
    }
}

impl TryFrom<Vec<Point>> for FanCurve {
    type Error = CurveError;
    fn try_from(points: Vec<Point>) -> Result<Self, CurveError> {
        Self::new(points)
    }
}

impl From<FanCurve> for Vec<Point> {
    fn from(c: FanCurve) -> Self {
        c.points
    }
}

/// Tuning of the controller loop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControllerParams {
    /// The speed is lowered only after the temperature dropped this many °C
    /// below the temperature that set the current speed.
    pub hysteresis: f32,
    /// Minimum time between a speed change and a following decrease.
    pub spindown_delay: Duration,
    /// Speed range accepted by the fans (NVML `min_max_fan_speed`).
    pub min_speed: u32,
    pub max_speed: u32,
}

impl Default for ControllerParams {
    fn default() -> Self {
        Self {
            hysteresis: 3.0,
            spindown_delay: Duration::from_secs(5),
            min_speed: 0,
            max_speed: 100,
        }
    }
}

/// Decides when to change the fan speed. Speeds go up immediately and come
/// down slowly (hysteresis + delay), so the fans never pulse.
#[derive(Debug, Clone)]
pub struct FanController {
    curve: FanCurve,
    params: ControllerParams,
    /// Current speed, the temperature that set it and when.
    current: Option<(u32, f32, Instant)>,
}

impl FanController {
    pub fn new(curve: FanCurve, params: ControllerParams) -> Self {
        Self {
            curve,
            params,
            current: None,
        }
    }

    pub fn target(&self, temp: f32) -> u32 {
        let s = self.curve.speed_at(temp).round() as u32;
        s.clamp(self.params.min_speed, self.params.max_speed)
    }

    /// The speed last decided, if any.
    pub fn current(&self) -> Option<u32> {
        self.current.map(|c| c.0)
    }

    /// Feeds a temperature reading; returns the new speed to write, or
    /// `None` when the fans should stay as they are.
    pub fn update(&mut self, temp: f32, now: Instant) -> Option<u32> {
        let target = self.target(temp);
        let change = match self.current {
            None => true,
            Some((cur, _, _)) if target > cur => true,
            Some((cur, ref_temp, since)) if target < cur => {
                ref_temp - temp >= self.params.hysteresis
                    && now.duration_since(since) >= self.params.spindown_delay
            }
            Some(_) => false,
        };
        if change {
            self.current = Some((target, temp, now));
            Some(target)
        } else {
            None
        }
    }

    /// Forgets the current speed (e.g. after the fans were reset elsewhere),
    /// so the next update writes again.
    pub fn invalidate(&mut self) {
        self.current = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve() -> FanCurve {
        FanCurve::new(vec![
            Point::new(70.0, 80.0),
            Point::new(30.0, 30.0),
            Point::new(50.0, 50.0),
        ])
        .unwrap()
    }

    #[test]
    fn interpolates_and_clamps() {
        let c = curve();
        assert_eq!(c.points()[0].temp, 30.0, "points are sorted");
        assert_eq!(c.speed_at(0.0), 30.0);
        assert_eq!(c.speed_at(30.0), 30.0);
        assert_eq!(c.speed_at(40.0), 40.0);
        assert_eq!(c.speed_at(60.0), 65.0);
        assert_eq!(c.speed_at(70.0), 80.0);
        // sLimit left every temperature past the last point at 0 %.
        assert_eq!(c.speed_at(95.0), 80.0);
    }

    #[test]
    fn descending_segments_work() {
        // sLimit computed these with unsigned arithmetic and underflowed.
        let c = FanCurve::new(vec![Point::new(30.0, 60.0), Point::new(60.0, 30.0)]).unwrap();
        assert_eq!(c.speed_at(45.0), 45.0);
    }

    #[test]
    fn rejects_bad_curves() {
        assert_eq!(FanCurve::new(vec![]), Err(CurveError::Empty));
        assert!(matches!(
            FanCurve::new(vec![Point::new(50.0, 101.0)]),
            Err(CurveError::Speed(_))
        ));
        assert!(matches!(
            FanCurve::new(vec![Point::new(50.0, 10.0), Point::new(50.0, 20.0)]),
            Err(CurveError::Duplicate(_))
        ));
    }

    #[test]
    fn toml_roundtrip_validates() {
        #[derive(Serialize, Deserialize)]
        struct W {
            curve: FanCurve,
        }
        let s = toml::to_string(&W { curve: curve() }).unwrap();
        let back: W = toml::from_str(&s).unwrap();
        assert_eq!(back.curve, curve());
        let bad = "curve = [{ temp = 10.0, speed = 200.0 }]";
        assert!(toml::from_str::<W>(bad).is_err());
    }

    #[test]
    fn controller_up_fast_down_slow() {
        let params = ControllerParams {
            hysteresis: 3.0,
            spindown_delay: Duration::from_secs(5),
            min_speed: 30,
            max_speed: 100,
        };
        let mut c = FanController::new(curve(), params);
        let t0 = Instant::now();
        let s = |secs| t0 + Duration::from_secs(secs);

        assert_eq!(c.update(50.0, s(0)), Some(50));
        assert_eq!(c.update(50.0, s(1)), None, "unchanged");
        assert_eq!(c.update(60.0, s(2)), Some(65), "up immediately");
        assert_eq!(c.update(58.0, s(10)), None, "within hysteresis");
        assert_eq!(c.update(56.0, s(3)), None, "too soon after the change");
        assert_eq!(
            c.update(56.0, s(8)),
            Some(59),
            "down after delay + hysteresis"
        );
        assert_eq!(
            c.update(10.0, s(20)),
            Some(30),
            "clamped to the fan minimum"
        );
    }

    #[test]
    fn controller_invalidate_rewrites() {
        let mut c = FanController::new(curve(), ControllerParams::default());
        let now = Instant::now();
        assert_eq!(c.update(50.0, now), Some(50));
        c.invalidate();
        assert_eq!(c.update(50.0, now), Some(50));
    }
}

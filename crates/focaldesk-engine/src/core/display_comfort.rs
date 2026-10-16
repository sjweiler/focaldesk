//! Display-only color accessibility transforms.
//!
//! These are deliberately kept out of the scene and capture pipelines: a
//! night-light setting changes the physical display, not the colors an app
//! submits to a screenshot or screen-sharing client.

use chrono::{Datelike, Local, Timelike};
use focaldesk_logging::flog_warn;
use focaldesk_settings_core::{
    AccessibilitySettings, ColorFilter, NightLightSchedule, load_settings, save_settings,
};

pub type ColorMatrix = [[f32; 3]; 3];

pub const IDENTITY: ColorMatrix = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

pub fn output_matrix(settings: &AccessibilitySettings, connector: &str) -> ColorMatrix {
    if settings
        .display_comfort_disabled_outputs
        .get(connector)
        .copied()
        .unwrap_or(false)
    {
        return IDENTITY;
    }
    multiply(
        ambient_brightness_matrix(settings),
        multiply(
            filter_matrix(settings.color_filter),
            night_light_matrix(settings),
        ),
    )
}

fn ambient_brightness_matrix(settings: &AccessibilitySettings) -> ColorMatrix {
    if !settings.ambient_light_enabled {
        return IDENTITY;
    }
    let Some(lux) = read_ambient_lux() else {
        return IDENTITY;
    };
    // 0–500 lux spans a dim room to a normally lit workspace. Above that we
    // leave the display at the configured manual brightness.
    let daylight = (lux / 500.0).clamp(0.0, 1.0);
    let gain = settings.ambient_min_brightness + (1.0 - settings.ambient_min_brightness) * daylight;
    [[gain, 0.0, 0.0], [0.0, gain, 0.0], [0.0, 0.0, gain]]
}

fn read_ambient_lux() -> Option<f32> {
    let devices = std::fs::read_dir("/sys/bus/iio/devices").ok()?;
    for device in devices.flatten() {
        let path = device.path();
        for name in ["in_illuminance_input", "in_illuminance_raw"] {
            let Ok(value) = std::fs::read_to_string(path.join(name)) else {
                continue;
            };
            if let Ok(lux) = value.trim().parse::<f32>() {
                if lux.is_finite() && lux >= 0.0 {
                    return Some(lux);
                }
            }
        }
    }
    None
}

pub fn active(settings: &AccessibilitySettings, connector: &str) -> bool {
    output_matrix(settings, connector) != IDENTITY
}

fn night_light_matrix(settings: &AccessibilitySettings) -> ColorMatrix {
    if !settings.night_light.enabled {
        return IDENTITY;
    }
    let intensity = night_light_intensity(settings);
    if intensity <= 0.0 {
        return IDENTITY;
    }
    let kelvin = settings.night_light.temperature_kelvin.clamp(1_000, 6_500) as f32;
    // A stable, bounded approximation based on the standard black-body RGB
    // fit. Green/blue are reduced relative to neutral 6500 K; red stays at
    // unity so the transform never boosts a display beyond its calibrated max.
    let temp = kelvin / 100.0;
    let red = if temp <= 66.0 {
        1.0
    } else {
        (329.698_73 * (temp - 60.0).powf(-0.133_204_76) / 255.0).clamp(0.0, 1.0)
    };
    let green = if temp <= 66.0 {
        (99.470_8 * temp.ln() - 161.119_57) / 255.0
    } else {
        (288.122_16 * (temp - 60.0).powf(-0.075_514_846)) / 255.0
    }
    .clamp(0.0, 1.0);
    let blue = if temp >= 66.0 {
        1.0
    } else if temp <= 19.0 {
        0.0
    } else {
        (138.517_73 * (temp - 10.0).ln() - 305.044_8) / 255.0
    }
    .clamp(0.0, 1.0);
    lerp_matrix(
        IDENTITY,
        [[red, 0.0, 0.0], [0.0, green, 0.0], [0.0, 0.0, blue]],
        intensity,
    )
}

fn night_light_intensity(settings: &AccessibilitySettings) -> f32 {
    match settings.night_light.schedule {
        NightLightSchedule::Manual => 1.0,
        NightLightSchedule::Custom => {
            let now = Local::now();
            let minute = (now.hour() * 60 + now.minute()) as u16;
            scheduled_intensity(
                minute,
                settings.night_light.custom_start_minutes,
                settings.night_light.custom_end_minutes,
                settings.night_light.transition_minutes,
            )
        }
        NightLightSchedule::Automatic => {
            let Some(latitude) = settings.night_light.latitude else {
                return custom_schedule_intensity(settings);
            };
            let Some(longitude) = settings.night_light.longitude else {
                return custom_schedule_intensity(settings);
            };
            let now = Local::now();
            let offset_minutes = now.offset().local_minus_utc() / 60;
            let (sunrise, sunset) =
                sunrise_sunset_minutes(now.ordinal(), latitude, longitude, offset_minutes);
            let minute = (now.hour() * 60 + now.minute()) as u16;
            scheduled_intensity(
                minute,
                sunset,
                sunrise,
                settings.night_light.transition_minutes,
            )
        }
    }
}

fn custom_schedule_intensity(settings: &AccessibilitySettings) -> f32 {
    let now = Local::now();
    let minute = (now.hour() * 60 + now.minute()) as u16;
    scheduled_intensity(
        minute,
        settings.night_light.custom_start_minutes,
        settings.night_light.custom_end_minutes,
        settings.night_light.transition_minutes,
    )
}

fn scheduled_intensity(minute: u16, start: u16, end: u16, fade_minutes: u16) -> f32 {
    let duration = (u32::from(end) + 1_440 - u32::from(start)) % 1_440;
    if duration == 0 {
        return 1.0;
    }
    let elapsed = (u32::from(minute) + 1_440 - u32::from(start)) % 1_440;
    if elapsed >= duration {
        return 0.0;
    }
    let fade = u32::from(fade_minutes);
    if fade == 0 {
        return 1.0;
    }
    let ramp_up = elapsed as f32 / fade as f32;
    let ramp_down = (duration - elapsed) as f32 / fade as f32;
    ramp_up.min(ramp_down).clamp(0.0, 1.0)
}

fn lerp_matrix(from: ColorMatrix, to: ColorMatrix, amount: f32) -> ColorMatrix {
    let amount = amount.clamp(0.0, 1.0);
    let mut out = IDENTITY;
    for row in 0..3 {
        for column in 0..3 {
            out[row][column] = from[row][column] + (to[row][column] - from[row][column]) * amount;
        }
    }
    out
}

/// Approximate local sunrise and sunset with the NOAA mean-solar-time model.
/// Polar-day/night cases fall back to a stable 7 PM–7 AM schedule.
pub fn sunrise_sunset_minutes(
    ordinal: u32,
    latitude: f64,
    longitude: f64,
    utc_offset_minutes: i32,
) -> (u16, u16) {
    let latitude = latitude.clamp(-89.8, 89.8).to_radians();
    let gamma = std::f64::consts::TAU / 365.0 * (f64::from(ordinal) - 1.0);
    let declination = 0.006_918 - 0.399_912 * gamma.cos() + 0.070_257 * gamma.sin()
        - 0.006_758 * (2.0 * gamma).cos()
        + 0.000_907 * (2.0 * gamma).sin()
        - 0.002_697 * (3.0 * gamma).cos()
        + 0.001_48 * (3.0 * gamma).sin();
    let equation_of_time = 229.18
        * (0.000_075 + 0.001_868 * gamma.cos()
            - 0.032_077 * gamma.sin()
            - 0.014_615 * (2.0 * gamma).cos()
            - 0.040_849 * (2.0 * gamma).sin());
    let zenith = 90.833_f64.to_radians();
    let cos_hour_angle = ((zenith.cos() / (latitude.cos() * declination.cos()))
        - latitude.tan() * declination.tan())
    .clamp(-1.0, 1.0);
    let hour_angle = cos_hour_angle.acos().to_degrees();
    let solar_noon = 720.0 - 4.0 * longitude - equation_of_time + f64::from(utc_offset_minutes);
    let sunrise = solar_noon - 4.0 * hour_angle;
    let sunset = solar_noon + 4.0 * hour_angle;
    if !sunrise.is_finite() || !sunset.is_finite() {
        return (420, 1_140);
    }
    (
        sunrise.round().clamp(0.0, 1_439.0) as u16,
        sunset.round().clamp(0.0, 1_439.0) as u16,
    )
}

fn filter_matrix(filter: ColorFilter) -> ColorMatrix {
    match filter {
        ColorFilter::Off => IDENTITY,
        ColorFilter::Grayscale => [
            [0.2126, 0.7152, 0.0722],
            [0.2126, 0.7152, 0.0722],
            [0.2126, 0.7152, 0.0722],
        ],
        // Color-blindness correction matrices. They preserve luminance while
        // increasing separation in the affected opponent-color channel.
        ColorFilter::Protanopia => [
            [0.567, 0.433, 0.0],
            [0.558, 0.442, 0.0],
            [0.0, 0.242, 0.758],
        ],
        ColorFilter::Deuteranopia => [[0.625, 0.375, 0.0], [0.70, 0.30, 0.0], [0.0, 0.30, 0.70]],
        ColorFilter::Tritanopia => [[0.95, 0.05, 0.0], [0.0, 0.433, 0.567], [0.0, 0.475, 0.525]],
    }
}

pub fn multiply(left: ColorMatrix, right: ColorMatrix) -> ColorMatrix {
    let mut out = [[0.0; 3]; 3];
    for row in 0..3 {
        for col in 0..3 {
            out[row][col] = (0..3)
                .map(|index| left[row][index] * right[index][col])
                .sum();
        }
    }
    out
}

impl super::desktop::DesktopState {
    pub(crate) fn toggle_night_light(&mut self) {
        let mut settings = load_settings();
        settings.accessibility.night_light.enabled = !settings.accessibility.night_light.enabled;
        let enabled = settings.accessibility.night_light.enabled;
        if let Err(error) = save_settings(&settings) {
            flog_warn!("could not save Night Light shortcut setting: {error}");
            return;
        }
        self.accessibility_settings = settings.accessibility;
        self.mark_all_outputs_full_damage(super::desktop::DamageSource::Unknown);
        super::desktop::notification_service_notify(
            "Night Light",
            if enabled { "Enabled" } else { "Disabled" },
            Some(std::time::Duration::from_secs(2)),
        );
    }

    pub(crate) fn cycle_color_filter(&mut self) {
        let mut settings = load_settings();
        settings.accessibility.color_filter = match settings.accessibility.color_filter {
            ColorFilter::Off => ColorFilter::Grayscale,
            ColorFilter::Grayscale => ColorFilter::Protanopia,
            ColorFilter::Protanopia => ColorFilter::Deuteranopia,
            ColorFilter::Deuteranopia => ColorFilter::Tritanopia,
            ColorFilter::Tritanopia => ColorFilter::Off,
        };
        let filter = settings.accessibility.color_filter;
        if let Err(error) = save_settings(&settings) {
            flog_warn!("could not save color filter shortcut setting: {error}");
            return;
        }
        self.accessibility_settings = settings.accessibility;
        self.mark_all_outputs_full_damage(super::desktop::DamageSource::Unknown);
        let label = match filter {
            ColorFilter::Off => "Off",
            ColorFilter::Grayscale => "Grayscale",
            ColorFilter::Protanopia => "Protanopia",
            ColorFilter::Deuteranopia => "Deuteranopia",
            ColorFilter::Tritanopia => "Tritanopia",
        };
        super::desktop::notification_service_notify(
            "Color filter",
            label,
            Some(std::time::Duration::from_secs(2)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_comfort_is_identity() {
        assert_eq!(
            output_matrix(&AccessibilitySettings::default(), "DP-1"),
            IDENTITY
        );
    }

    #[test]
    fn grayscale_has_equal_rows() {
        let settings = AccessibilitySettings {
            color_filter: ColorFilter::Grayscale,
            ..AccessibilitySettings::default()
        };
        let matrix = output_matrix(&settings, "DP-1");
        assert_eq!(matrix[0], matrix[1]);
        assert_eq!(matrix[1], matrix[2]);
    }

    #[test]
    fn disabled_connector_opt_out_wins() {
        let mut settings = AccessibilitySettings {
            color_filter: ColorFilter::Tritanopia,
            ..AccessibilitySettings::default()
        };
        settings
            .display_comfort_disabled_outputs
            .insert("DP-1".into(), true);
        assert_eq!(output_matrix(&settings, "DP-1"), IDENTITY);
    }

    #[test]
    fn solar_schedule_is_plausible_at_the_equator() {
        let (sunrise, sunset) = sunrise_sunset_minutes(80, 0.0, 0.0, 0);
        assert!((330..=450).contains(&sunrise));
        assert!((990..=1_110).contains(&sunset));
    }

    #[test]
    fn scheduled_fade_is_smooth_at_both_boundaries() {
        assert_eq!(scheduled_intensity(1_140, 1_140, 420, 30), 0.0);
        assert!((scheduled_intensity(1_155, 1_140, 420, 30) - 0.5).abs() < 0.01);
        assert!((scheduled_intensity(405, 1_140, 420, 30) - 0.5).abs() < 0.01);
        assert_eq!(scheduled_intensity(420, 1_140, 420, 30), 0.0);
    }
}

use std::f32::consts::PI;

pub const SAMPLE_RATE: u32 = 44_100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UiSound {
    Hover,
    Select,
    OpenFolder,
    CloseFolder,
    Error,
    Success,
}

pub fn generate_ui_sound(sound: UiSound) -> Vec<f32> {
    match sound {
        UiSound::Hover => tick(800.0, 0.025, 0.10),
        UiSound::Select => sweep(900.0, 1500.0, 0.050, 0.30),
        UiSound::OpenFolder => arpeggio(&[700.0, 950.0, 1250.0], 0.055, 0.22),
        UiSound::CloseFolder => arpeggio(&[1250.0, 950.0, 700.0], 0.055, 0.22),
        UiSound::Error => double_buzz(170.0, 0.080, 0.35),
        UiSound::Success => arpeggio(&[880.0, 1320.0], 0.090, 0.25),
    }
}

fn tick(freq: f32, duration: f32, volume: f32) -> Vec<f32> {
    let samples = (duration * SAMPLE_RATE as f32) as usize;
    let mut out = Vec::with_capacity(samples);

    for i in 0..samples {
        let t = i as f32 / SAMPLE_RATE as f32;
        let env = (-90.0 * t).exp();
        let s = (2.0 * PI * freq * t).sin() * env * volume;
        out.push(s);
    }

    out
}

fn sweep(start_freq: f32, end_freq: f32, duration: f32, volume: f32) -> Vec<f32> {
    let samples = (duration * SAMPLE_RATE as f32) as usize;
    let click_samples = (0.002 * SAMPLE_RATE as f32) as usize;

    let mut out = Vec::with_capacity(samples);
    let mut rng: u32 = 0x12345678;

    let mut phase = 0.0;

    for i in 0..samples {
        let t = i as f32 / SAMPLE_RATE as f32;
        let progress = i as f32 / samples as f32;

        let freq = start_freq + (end_freq - start_freq) * progress;
        phase += 2.0 * PI * freq / SAMPLE_RATE as f32;

        let env = (-35.0 * t).exp();
        let tone = phase.sin() * env * volume;

        let click = if i < click_samples {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;

            let noise = (rng as f32 / u32::MAX as f32) * 2.0 - 1.0;
            noise * 0.18 * (1.0 - i as f32 / click_samples as f32)
        } else {
            0.0
        };

        out.push(tone + click);
    }

    out
}

fn arpeggio(notes: &[f32], note_duration: f32, volume: f32) -> Vec<f32> {
    let mut out = Vec::new();

    for &freq in notes {
        let samples = (note_duration * SAMPLE_RATE as f32) as usize;

        for i in 0..samples {
            let t = i as f32 / SAMPLE_RATE as f32;
            let env = (-28.0 * t).exp();

            let fundamental = (2.0 * PI * freq * t).sin();
            let harmonic = (2.0 * PI * freq * 2.0 * t).sin() * 0.18;

            out.push((fundamental + harmonic) * env * volume);
        }
    }

    out
}

fn double_buzz(freq: f32, buzz_duration: f32, volume: f32) -> Vec<f32> {
    let mut out = Vec::new();

    for buzz in 0..2 {
        let samples = (buzz_duration * SAMPLE_RATE as f32) as usize;

        for i in 0..samples {
            let t = i as f32 / SAMPLE_RATE as f32;
            let env = (-18.0 * t).exp();

            let squareish = (2.0 * PI * freq * t).sin().signum() * 0.6
                + (2.0 * PI * freq * 2.0 * t).sin() * 0.25;

            out.push(squareish * env * volume);
        }

        if buzz == 0 {
            let gap = (0.045 * SAMPLE_RATE as f32) as usize;
            out.extend(std::iter::repeat(0.0).take(gap));
        }
    }

    out
}

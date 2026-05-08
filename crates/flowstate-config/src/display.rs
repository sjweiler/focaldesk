#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DisplayConfig {
    pub name: String,
    pub enabled: bool,

    pub mode_width: i32,
    pub mode_height: i32,
    pub refresh_mhz: i32,

    pub scale: f64,

    pub logical_x: i32,
    pub logical_y: i32,

    pub physical_width_mm: Option<i32>,
    pub physical_height_mm: Option<i32>,

    pub primary: bool,
    pub transform: DisplayTransform,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum DisplayTransform {
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
}


impl DisplayConfig {
    pub fn dpi(&self) -> Option<f64> {
        let w_mm = self.physical_width_mm?;
        let h_mm = self.physical_height_mm?;

        if w_mm <= 0 || h_mm <= 0 {
            return None;
        }

        let w_in = w_mm as f64 / 25.4;
        let h_in = h_mm as f64 / 25.4;

        let px_diag = ((self.mode_width.pow(2) + self.mode_height.pow(2)) as f64).sqrt();
        let in_diag = (w_in.powi(2) + h_in.powi(2)).sqrt();

        Some(px_diag / in_diag)
    }

    pub fn logical_width(&self) -> f64 {
        self.mode_width as f64 / self.scale
    }

    pub fn logical_height(&self) -> f64 {
        self.mode_height as f64 / self.scale
    }
}

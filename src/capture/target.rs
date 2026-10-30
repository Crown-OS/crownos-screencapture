use std::fmt;
use std::str::FromStr;

use super::CaptureError;

/// A monitor as the compositor advertises it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputInfo {
    pub name: String,
    pub description: String,
    pub logical_width: u32,
    pub logical_height: u32,
}

/// A toplevel window from `ext-foreign-toplevel-list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub identifier: String,
    pub title: String,
    pub app_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureTarget {
    Output(String),
    Window(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorMode {
    #[default]
    Hidden,
    Painted,
}

impl CursorMode {
    pub fn from_visibility(visible: bool) -> Self {
        if visible { Self::Painted } else { Self::Hidden }
    }
}

/// A rectangle in logical pixels relative to the captured output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// A rectangle in buffer pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// Rounds the size down to even numbers, as 4:2:0 video encoders require.
    pub fn even(self) -> Self {
        Self {
            width: self.width & !1,
            height: self.height & !1,
            ..self
        }
    }
}

impl Region {
    /// Maps the region onto a buffer of `buffer_width` × `buffer_height` that shows `logical_width`
    /// × `logical_height` logical pixels, clamped to the buffer.
    pub fn to_pixels(
        self,
        logical_width: u32,
        logical_height: u32,
        buffer_width: u32,
        buffer_height: u32,
    ) -> PixelRect {
        let scale_x = f64::from(buffer_width) / f64::from(logical_width.max(1));
        let scale_y = f64::from(buffer_height) / f64::from(logical_height.max(1));
        let x = ((f64::from(self.x) * scale_x).round() as u32).min(buffer_width);
        let y = ((f64::from(self.y) * scale_y).round() as u32).min(buffer_height);
        let width = ((f64::from(self.width) * scale_x).round() as u32).min(buffer_width - x);
        let height = ((f64::from(self.height) * scale_y).round() as u32).min(buffer_height - y);
        PixelRect {
            x,
            y,
            width,
            height,
        }
    }
}

impl fmt::Display for Region {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{},{},{},{}",
            self.x, self.y, self.width, self.height
        )
    }
}

impl FromStr for Region {
    type Err = CaptureError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || CaptureError::InvalidRegion(text.to_owned());
        let mut numbers = text.split(',').map(|part| part.trim().parse::<u32>());
        let mut next = || numbers.next().and_then(Result::ok).ok_or_else(invalid);
        let region = Self {
            x: next()?,
            y: next()?,
            width: next()?,
            height: next()?,
        };
        if region.width == 0 || region.height == 0 {
            return Err(invalid());
        }
        Ok(region)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_region() {
        let region: Region = "10, 20,300,400".parse().unwrap();
        assert_eq!(
            region,
            Region {
                x: 10,
                y: 20,
                width: 300,
                height: 400
            }
        );
        assert_eq!(region.to_string().parse::<Region>().unwrap(), region);
    }

    #[test]
    fn rejects_empty_region() {
        assert!("1,2,0,4".parse::<Region>().is_err());
        assert!("1,2,3".parse::<Region>().is_err());
    }

    #[test]
    fn scales_region_to_buffer() {
        let region = Region {
            x: 100,
            y: 50,
            width: 200,
            height: 100,
        };
        let rect = region.to_pixels(1920, 1080, 3840, 2160);
        assert_eq!(
            rect,
            PixelRect {
                x: 200,
                y: 100,
                width: 400,
                height: 200
            }
        );
    }

    #[test]
    fn clamps_region_to_buffer() {
        let region = Region {
            x: 1900,
            y: 1000,
            width: 200,
            height: 200,
        };
        let rect = region.to_pixels(1920, 1080, 1920, 1080);
        assert_eq!(
            rect,
            PixelRect {
                x: 1900,
                y: 1000,
                width: 20,
                height: 80
            }
        );
    }

    #[test]
    fn rounds_to_even() {
        assert_eq!(
            PixelRect {
                x: 1,
                y: 1,
                width: 5,
                height: 7
            }
            .even(),
            PixelRect {
                x: 1,
                y: 1,
                width: 4,
                height: 6
            }
        );
    }
}

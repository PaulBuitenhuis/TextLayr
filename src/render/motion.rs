//! Popup entrance/exit motion: easing, per-phase transforms, and the
//! scale/offset/alpha a state is drawn with.

use crate::config::{Easing, MotionStyle, PopupEffect};

/// A whole-popup transform: scale, offset and alpha.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PopupTransform {
    pub scale: f32,
    pub dx: f32,
    pub dy: f32,
    pub alpha: f32,
}

impl PopupTransform {
    pub const IDENTITY: Self = Self {
        scale: 1.0,
        dx: 0.0,
        dy: 0.0,
        alpha: 1.0,
    };

    pub fn is_identity(&self) -> bool {
        (self.scale - 1.0).abs() < 1e-4
            && self.dx.abs() < 0.01
            && self.dy.abs() < 0.01
            && (self.alpha - 1.0).abs() < 1e-4
    }
}

fn ease(easing: Easing, p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    match easing {
        Easing::Linear => p,
        Easing::EaseOut => 1.0 - (1.0 - p).powi(3),
        Easing::EaseInOut => p * p * (3.0 - 2.0 * p),
        Easing::Spring => {
            const C1: f32 = 1.701_58;
            let c3 = C1 + 1.0;
            1.0 + c3 * (p - 1.0).powi(3) + C1 * (p - 1.0).powi(2)
        }
    }
}

/// The transform for a popup at `phase` (0 = just appeared, 1 = settled).
/// `font_px` converts the slide distance (in em) to pixels.
pub fn popup_transform(motion: &MotionStyle, phase: f32, font_px: f32) -> PopupTransform {
    if motion.popup == PopupEffect::None || motion.duration_ms == 0 || phase >= 1.0 {
        return PopupTransform::IDENTITY;
    }
    let eased = ease(motion.easing, phase);
    let amount = motion.intensity.max(0.0);
    let travel = amount * font_px * (1.0 - eased);
    match motion.popup {
        PopupEffect::None => PopupTransform::IDENTITY,
        PopupEffect::Pop => PopupTransform {
            scale: (1.0 - amount * (1.0 - eased)).max(0.05),
            ..PopupTransform::IDENTITY
        },
        PopupEffect::SlideUp => PopupTransform {
            dy: travel,
            ..PopupTransform::IDENTITY
        },
        PopupEffect::SlideDown => PopupTransform {
            dy: -travel,
            ..PopupTransform::IDENTITY
        },
        PopupEffect::SlideLeft => PopupTransform {
            dx: travel,
            ..PopupTransform::IDENTITY
        },
        PopupEffect::SlideRight => PopupTransform {
            dx: -travel,
            ..PopupTransform::IDENTITY
        },
        PopupEffect::Fade => PopupTransform {
            alpha: eased.clamp(0.0, 1.0),
            ..PopupTransform::IDENTITY
        },
    }
}

/// The transform for a popup leaving at `phase` (0 = still settled, 1 = gone).
pub fn popup_exit_transform(motion: &MotionStyle, phase: f32, font_px: f32) -> PopupTransform {
    if motion.exit == PopupEffect::None || motion.exit_duration_ms == 0 {
        return PopupTransform::IDENTITY;
    }
    let eased = ease(motion.easing, phase);
    let amount = motion.intensity.max(0.0);
    let travel = amount * font_px * eased;
    let alpha = (1.0 - eased).clamp(0.0, 1.0);
    match motion.exit {
        PopupEffect::None => PopupTransform::IDENTITY,
        PopupEffect::Pop => PopupTransform {
            scale: (1.0 - amount * eased).max(0.05),
            alpha,
            ..PopupTransform::IDENTITY
        },
        PopupEffect::SlideUp => PopupTransform {
            dy: -travel,
            alpha,
            ..PopupTransform::IDENTITY
        },
        PopupEffect::SlideDown => PopupTransform {
            dy: travel,
            alpha,
            ..PopupTransform::IDENTITY
        },
        PopupEffect::SlideLeft => PopupTransform {
            dx: -travel,
            alpha,
            ..PopupTransform::IDENTITY
        },
        PopupEffect::SlideRight => PopupTransform {
            dx: travel,
            alpha,
            ..PopupTransform::IDENTITY
        },
        PopupEffect::Fade => PopupTransform {
            alpha,
            ..PopupTransform::IDENTITY
        },
    }
}

/// Which phase of a popup's life a frame is in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MotionPhase {
    Entrance(f32),
    Settled,
    Exit(f32),
    Hidden,
}

impl MotionPhase {
    pub fn transform(self, motion: &MotionStyle, font_px: f32) -> PopupTransform {
        match self {
            MotionPhase::Entrance(p) => popup_transform(motion, p, font_px),
            MotionPhase::Exit(p) => popup_exit_transform(motion, p, font_px),
            MotionPhase::Settled | MotionPhase::Hidden => PopupTransform::IDENTITY,
        }
    }

    pub fn is_hidden(self) -> bool {
        matches!(self, MotionPhase::Hidden)
    }
}

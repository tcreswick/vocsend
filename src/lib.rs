//! vocsend: send small amounts of data between computers by sound.
//!
//! The library holds everything that does not touch a sound card, so the
//! protocol can be driven from any source of audio samples.

pub mod crypto;
pub mod dsp;
pub mod frame;
pub mod pack;
pub mod rs;
pub mod synth;
pub mod modem;
pub mod payload;
pub mod plan;
pub mod resample;

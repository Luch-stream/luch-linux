//! luch-win: the Luch sender for Windows, built the way luchd's fastest mode (luch-kms) is: one thread takes the
//! newest picture from the GPU, converts it to NV12 on the GPU, hands it to the hardware encoder and sends each
//! access unit the moment it is out. docs/windows.md says why each part is the way it is.
//!
//! The parts that are not Windows (the pipeline loop, pacing, the UDP sender, the control channel, the encoder
//! settings, casting files and what Explorer's menu is made of) build and are tested on Linux too; `win` holds the
//! Windows API calls.

pub mod cast;
pub mod cli;
pub mod control;
pub mod gather;
pub mod pace;
pub mod pipeline;
pub mod settings;
pub mod shell;
pub mod tx;

#[cfg(windows)]
pub mod win;

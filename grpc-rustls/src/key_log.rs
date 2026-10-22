/*
 * Copyright (c) 2016 Joseph Birr-Pixton <jpixton@gmail.com>
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 * NOTE: This file contains modifications by the gRPC authors; see revision
 * history for details.
 *
 */

use core::fmt::Debug;
use core::fmt::Formatter;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use rustls::KeyLog;

// Internal mutable state for KeyLogFile
struct KeyLogFileInner {
    file: Option<File>,
    buf: Vec<u8>,
}

impl KeyLogFileInner {
    fn new(path: &PathBuf) -> Self {
        let file = match OpenOptions::new().append(true).create(true).open(path) {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("unable to create key log file {path:?}: {e}");
                None
            }
        };

        Self {
            file,
            buf: Vec::new(),
        }
    }

    fn try_write(&mut self, label: &str, client_random: &[u8], secret: &[u8]) -> io::Result<()> {
        let Some(file) = &mut self.file else {
            return Ok(());
        };

        self.buf.clear();
        write!(self.buf, "{label} ")?;
        for b in client_random.iter() {
            write!(self.buf, "{b:02x}")?;
        }
        write!(self.buf, " ")?;
        for b in secret.iter() {
            write!(self.buf, "{b:02x}")?;
        }
        writeln!(self.buf)?;
        file.write_all(&self.buf)
    }
}

impl Debug for KeyLogFileInner {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KeyLogFileInner")
            // Note: we omit self.buf deliberately as it may contain key data.
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

/// [`KeyLog`] implementation that opens a file whose name is
/// given to the constructor, and writes keys into it.
///
/// If such a file cannot be opened, or cannot be written then
/// this does nothing but logs errors.
pub struct KeyLogFile(Mutex<KeyLogFileInner>);

impl KeyLogFile {
    /// Makes a new `KeyLogFile`.  The environment variable is
    /// inspected and the named file is opened during this call.
    pub fn new(path: &PathBuf) -> Self {
        Self(Mutex::new(KeyLogFileInner::new(path)))
    }
}

impl KeyLog for KeyLogFile {
    fn log(&self, label: &str, client_random: &[u8], secret: &[u8]) {
        match self
            .0
            .lock()
            .unwrap()
            .try_write(label, client_random, secret)
        {
            Ok(()) => {}
            Err(e) => {
                eprintln!("error writing to key log file: {e}");
            }
        }
    }
}

impl Debug for KeyLogFile {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self.0.try_lock() {
            Ok(key_log_file) => write!(f, "{key_log_file:?}"),
            Err(_) => write!(f, "KeyLogFile {{ <locked> }}"),
        }
    }
}

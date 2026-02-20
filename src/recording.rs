//! Recording and replay module for t§ransmissions
//!
//! Saves detected transmissions to disk and can replay them for testing.

use crate::detector::Transmission;
use num_complex::Complex;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Metadata for a recorded transmission
#[derive(Clone, Debug)]
pub struct RecordingMetadata {
    pub id: u64,
    pub sample_count: usize,
    pub peak_level: f32,
    pub avg_level: f32,
    pub timestamp_ms: u64,
}

/// Save a transmission to a file
/// 
/// File format: .iq32 (interleaved 32-bit float I/Q samples)
/// Filename: tx_{id}_{timestamp}_{samples}.iq32
pub fn save_transmission(
    transmission: &Transmission,
    output_dir: &Path,
) -> std::io::Result<PathBuf> {
    // Ensure directory exists
    fs::create_dir_all(output_dir)?;
    
    // Generate filename with metadata
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    
    let filename = format!(
        "tx_{:06}_{}_{}samples.iq32",
        transmission.id,
        timestamp,
        transmission.samples.len()
    );
    
    let filepath = output_dir.join(&filename);
    
    // Write IQ samples as interleaved f32
    let file = File::create(&filepath)?;
    let mut writer = BufWriter::new(file);
    
    for sample in &transmission.samples {
        writer.write_all(&sample.re.to_le_bytes())?;
        writer.write_all(&sample.im.to_le_bytes())?;
    }
    
    writer.flush()?;
    
    // Also save metadata as a companion .meta file
    let meta_path = filepath.with_extension("meta");
    let meta_content = format!(
        "id={}\nsamples={}\npeak={:.6}\navg={:.6}\ntimestamp={}\n",
        transmission.id,
        transmission.samples.len(),
        transmission.peak_level,
        transmission.avg_level,
        timestamp
    );
    fs::write(meta_path, meta_content)?;
    
    Ok(filepath)
}

/// Load a transmission from a .iq32 file
pub fn load_transmission(filepath: &Path) -> std::io::Result<Transmission> {
    let file = File::open(filepath)?;
    let file_size = file.metadata()?.len() as usize;
    let sample_count = file_size / 8; // 4 bytes for I + 4 bytes for Q
    
    let mut reader = BufReader::new(file);
    let mut samples = Vec::with_capacity(sample_count);
    
    let mut buf = [0u8; 8];
    while reader.read_exact(&mut buf).is_ok() {
        let re = f32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let im = f32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        samples.push(Complex::new(re, im));
    }
    
    // Try to load metadata
    let meta_path = filepath.with_extension("meta");
    let (id, peak_level, avg_level) = if meta_path.exists() {
        parse_metadata(&meta_path).unwrap_or((0, 0.0, 0.0))
    } else {
        // Calculate from samples
        let peak = samples.iter()
            .map(|s| (s.re * s.re + s.im * s.im).sqrt())
            .fold(0.0f32, f32::max);
        let avg = samples.iter()
            .map(|s| (s.re * s.re + s.im * s.im).sqrt())
            .sum::<f32>() / samples.len().max(1) as f32;
        (0, peak, avg)
    };
    
    Ok(Transmission {
        samples,
        start_sample: 0,
        peak_level,
        avg_level,
        id,
    })
}

fn parse_metadata(path: &Path) -> Option<(u64, f32, f32)> {
    let content = fs::read_to_string(path).ok()?;
    let mut id = 0u64;
    let mut peak = 0.0f32;
    let mut avg = 0.0f32;
    
    for line in content.lines() {
        if let Some((key, value)) = line.split_once('=') {
            match key {
                "id" => id = value.parse().unwrap_or(0),
                "peak" => peak = value.parse().unwrap_or(0.0),
                "avg" => avg = value.parse().unwrap_or(0.0),
                _ => {}
            }
        }
    }
    
    Some((id, peak, avg))
}

/// List all .iq32 files in a directory
pub fn list_recordings(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut recordings = Vec::new();
    
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "iq32").unwrap_or(false) {
                recordings.push(path);
            }
        }
    }
    
    // Sort by filename (which includes ID and timestamp)
    recordings.sort();
    
    Ok(recordings)
}

/// Recording manager that handles saving transmissions in a background-friendly way
pub struct Recorder {
    output_dir: PathBuf,
    count: u64,
}

impl Recorder {
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
            count: 0,
        }
    }
    
    /// Record a transmission, returns the file path on success
    pub fn record(&mut self, transmission: &Transmission) -> std::io::Result<PathBuf> {
        let path = save_transmission(transmission, &self.output_dir)?;
        self.count += 1;
        Ok(path)
    }
    
    /// Get the number of transmissions recorded
    pub fn count(&self) -> u64 {
        self.count
    }
    
    /// Get the output directory
    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }
}

/// Replay manager that loads and iterates through recorded transmissions
pub struct Replayer {
    recordings: Vec<PathBuf>,
    current_index: usize,
    loop_enabled: bool,
}

impl Replayer {
    pub fn new(input_dir: impl AsRef<Path>, loop_enabled: bool) -> std::io::Result<Self> {
        let recordings = list_recordings(input_dir.as_ref())?;
        
        if recordings.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("No .iq32 files found in {:?}", input_dir.as_ref()),
            ));
        }
        
        println!("Found {} recordings to replay", recordings.len());
        
        Ok(Self {
            recordings,
            current_index: 0,
            loop_enabled,
        })
    }
    
    /// Get the next transmission to replay
    pub fn next(&mut self) -> Option<std::io::Result<Transmission>> {
        if self.current_index >= self.recordings.len() {
            if self.loop_enabled {
                self.current_index = 0;
            } else {
                return None;
            }
        }
        
        let path = &self.recordings[self.current_index];
        self.current_index += 1;
        
        Some(load_transmission(path))
    }
    
    /// Get the number of recordings available
    pub fn count(&self) -> usize {
        self.recordings.len()
    }
    
    /// Check if replay is complete (only relevant when not looping)
    pub fn is_complete(&self) -> bool {
        !self.loop_enabled && self.current_index >= self.recordings.len()
    }
    
    /// Reset to the beginning
    pub fn reset(&mut self) {
        self.current_index = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_save_and_load() {
        let dir = tempdir().unwrap();
        
        let transmission = Transmission {
            samples: vec![
                Complex::new(0.5, -0.5),
                Complex::new(0.1, 0.2),
                Complex::new(-0.3, 0.4),
            ],
            start_sample: 1000,
            peak_level: 0.707,
            avg_level: 0.35,
            id: 42,
        };
        
        let path = save_transmission(&transmission, dir.path()).unwrap();
        assert!(path.exists());
        
        let loaded = load_transmission(&path).unwrap();
        assert_eq!(loaded.samples.len(), 3);
        assert_eq!(loaded.id, 42);
        assert!((loaded.peak_level - 0.707).abs() < 0.001);
        
        // Check sample values
        assert!((loaded.samples[0].re - 0.5).abs() < 0.0001);
        assert!((loaded.samples[0].im - (-0.5)).abs() < 0.0001);
    }
    
    #[test]
    fn test_list_recordings() {
        let dir = tempdir().unwrap();
        
        // Save a few transmissions
        for i in 0..3 {
            let tx = Transmission {
                samples: vec![Complex::new(0.1, 0.1); 100],
                start_sample: 0,
                peak_level: 0.14,
                avg_level: 0.14,
                id: i,
            };
            save_transmission(&tx, dir.path()).unwrap();
        }
        
        let recordings = list_recordings(dir.path()).unwrap();
        assert_eq!(recordings.len(), 3);
    }

    #[test]
    fn test_replayer_round_trip_two_transmissions() {
        let dir = tempdir().unwrap();

        let tx1 = Transmission {
            samples: vec![Complex::new(1.0, 2.0), Complex::new(3.0, 4.0)],
            start_sample: 0,
            peak_level: 4.0,
            avg_level: 2.5,
            id: 1,
        };
        let tx2 = Transmission {
            samples: vec![Complex::new(0.5, -0.5), Complex::new(-1.0, 0.1)],
            start_sample: 100,
            peak_level: 1.0,
            avg_level: 0.6,
            id: 2,
        };

        let mut recorder = Recorder::new(dir.path().to_path_buf());
        recorder.record(&tx1).unwrap();
        recorder.record(&tx2).unwrap();
        assert_eq!(recorder.count(), 2);

        let mut replayer = Replayer::new(dir.path().to_path_buf(), false).unwrap();
        assert_eq!(replayer.count(), 2);
        assert!(!replayer.is_complete());

        let loaded1 = replayer.next().unwrap().unwrap();
        assert_eq!(loaded1.samples.len(), 2);
        assert!((loaded1.samples[0].re - 1.0).abs() < 1e-6, "tx1 sample[0].re mismatch");
        assert!((loaded1.samples[0].im - 2.0).abs() < 1e-6, "tx1 sample[0].im mismatch");
        assert!((loaded1.samples[1].re - 3.0).abs() < 1e-6, "tx1 sample[1].re mismatch");

        let loaded2 = replayer.next().unwrap().unwrap();
        assert_eq!(loaded2.samples.len(), 2);
        assert!((loaded2.samples[0].re - 0.5).abs() < 1e-6, "tx2 sample[0].re mismatch");
        assert!((loaded2.samples[0].im - (-0.5)).abs() < 1e-6, "tx2 sample[0].im mismatch");

        assert!(replayer.is_complete());
        assert!(replayer.next().is_none(), "expected None after all recordings consumed");
    }

    #[test]
    fn test_replayer_loop_restarts() {
        let dir = tempdir().unwrap();

        let tx = Transmission {
            samples: vec![Complex::new(1.0, 0.0)],
            start_sample: 0,
            peak_level: 1.0,
            avg_level: 1.0,
            id: 1,
        };
        save_transmission(&tx, dir.path()).unwrap();

        let mut replayer = Replayer::new(dir.path().to_path_buf(), true).unwrap();

        // In loop mode, should never return None
        for _ in 0..5 {
            assert!(replayer.next().is_some(), "expected Some in loop mode");
            assert!(!replayer.is_complete(), "is_complete should be false in loop mode");
        }
    }
}

/// Line reader for processing output line by line
pub struct LineReader {
    buffer: Vec<u8>,
}

impl LineReader {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
        }
    }

    /// Add data and extract complete lines
    pub fn add_data(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(data);
        self.extract_lines()
    }

    /// Extract all complete lines from buffer
    fn extract_lines(&mut self) -> Vec<Vec<u8>> {
        let mut lines = Vec::new();

        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            lines.push(line);
        }

        lines
    }

}

impl Default for LineReader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {}

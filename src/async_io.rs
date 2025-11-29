use memchr::memchr;
use std::cmp::min;
use std::io;
use std::io::{Read, Write};

#[derive(Debug, Clone, Default)]
pub struct ReadBuf {
	buf: Box<[u8]>,
	prod_head: usize,
	cons_tail: usize,
	len: usize,
}
impl ReadBuf {
	pub fn new(cap: usize) -> Self {
		let b = vec![0u8; cap];
		let buf = b.into();
		Self { buf, prod_head: 0, cons_tail: 0, len: 0 }
	}
	pub fn get_write_buf(&mut self) -> &mut [u8] {
		let head = self.prod_head;
		let tail = self.cons_tail;
		let buf_head = if tail < head {
			self.capacity()
		} else if head == tail {
			if self.len == 0 {
				self.capacity()
			}else {
				head
			}
		} else {
			tail
		};
		&mut self.buf[head..buf_head]
	}
	pub fn available_space(&self) -> usize {
		self.capacity() - self.len
	}
	pub fn write_len(&mut self, size: usize) {
		let available_size = self.get_write_buf().len();
		debug_assert!(size <= available_size);
		self.prod_head += size;
		self.prod_head %= self.capacity();
		self.len += size;
	}
	#[inline]
	pub fn capacity(&self) -> usize {
		self.buf.len()
	}

	pub fn is_empty(&self) -> bool {
		self.len == 0
	}

	pub fn read_line(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
		if self.is_empty() {
			return Ok(0);
		}
		let head = self.prod_head;
		let tail = self.cons_tail;
		let (buf1, buf2) = if head <= tail {
			(&self.buf[tail..self.capacity()], &self.buf[0..head])
		} else {
			(&self.buf[tail..head], &self.buf[0..0])
		};
		let old = buf.len();
		if let Some(idx) = memchr(b'\n', buf1) {
			buf.resize(old + idx + 1, 0);
			let _ = self.read(&mut buf.as_mut_slice()[old..old + idx + 1]);
			Ok(buf.len())
		} else if let Some(idx) = memchr(b'\n', buf2) {
			buf.resize(old + buf1.len() + idx + 1, 0);
			let _ = self.read(&mut buf.as_mut_slice()[old..old + idx + 1]);
			Ok(buf.len())
		} else {
			buf.resize(old + buf1.len() + buf2.len(), 0);
			let _ = self.read(&mut buf.as_mut_slice()[old..]);
			Err(io::ErrorKind::WouldBlock.into())
		}
	}
}
impl Read for ReadBuf {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		if self.len == 0 {
			return Ok(0);
		}
		let head = self.prod_head;
		let tail = self.cons_tail;
		let size = min(self.len, buf.len());
		// 判断数据是否连续（非满缓冲区且 tail <= head）
		if tail <= head && head - tail == self.len {
			// 数据连续在 [tail..head]
			buf[..size].copy_from_slice(&self.buf[tail..tail + size]);
			self.cons_tail += size;
		} else {
			// 数据分两段（可能满缓冲区或回绕）
			let first_len = min(size, self.capacity() - tail);
			buf[..first_len].copy_from_slice(&self.buf[tail..tail + first_len]);
			if first_len < size {
				let second_len = size - first_len;
				buf[first_len..size].copy_from_slice(&self.buf[0..second_len]);
			}
			self.cons_tail = (tail + size) % self.capacity();
		}
		self.len -= size;
		Ok(size)
	}
}
impl Write for ReadBuf {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		let size = min(self.available_space(), buf.len());
		if size == 0 {
			return Ok(0);
		}
		let mut b = self.get_write_buf();
		let first_len = min(buf.len(), b.len());
		b[..first_len].copy_from_slice(&buf[..first_len]);
		if b.len() < size {
			let copy_size = b.len();
			self.write_len(copy_size);
			b = self.get_write_buf();
			b.copy_from_slice(&buf[copy_size..]);
			let copy_size = b.len();
			self.write_len(copy_size);
		} else {
			self.write_len(size);
		}

		Ok(size)
	}

	fn flush(&mut self) -> io::Result<()> {
		Ok(())
	}
}

#[cfg(test)]
mod tests {

	use super::*;
	#[test]
	fn test_read_buf() {
		let mut read_tmp = [0u8; 32];

		let mut read_buf = ReadBuf::new(16);
		let buf = read_buf.get_write_buf();
		assert_eq!(buf.len(), 16);
		buf.copy_from_slice(b"0123456789012345");
		read_buf.write_len(10);
		read_buf.read(&mut read_tmp).expect("read bug");
		assert_eq!(&read_tmp[0..10], b"0123456789");
		// 现在缓冲区有6字节未提交数据（索引10-15），以及10字节空闲空间（因为已读取10字节）
		// 提交剩余的6字节
		read_buf.write_len(6);
		read_buf.read(&mut read_tmp).expect("read bug");
		assert_eq!(&read_tmp[0..6], b"012345");
		// 现在缓冲区为空，写入整个容量
		let buf = read_buf.get_write_buf();
		assert_eq!(buf.len(), 16);
		buf.copy_from_slice(b"0123456789012345");
		read_buf.write_len(16);
		assert_eq!(read_buf.read(&mut read_tmp).expect("read bug"), 16);
		assert_eq!(&read_tmp[0..16], b"0123456789012345");
	}
	#[test]
	fn test_read_buf_with_newline() {
		let mut read_tmp = Vec::with_capacity(8);

		let mut read_buf = ReadBuf::new(16);
		let buf = read_buf.get_write_buf();
		assert_eq!(buf.len(), 16);
		buf.copy_from_slice(b"012\n345678901234");
		read_buf.write_len(16);

		assert_eq!(read_buf.read_line(&mut read_tmp).unwrap(), 4);
		assert_eq!(read_tmp.as_slice(), b"012\n");
		read_tmp.clear();
		assert!(read_buf.read_line(&mut read_tmp).is_err());
		assert_eq!(read_tmp.as_slice(), b"345678901234");
		read_buf.write_len(6);
		assert_eq!(read_buf.read_line(&mut read_tmp).unwrap(), 16);
		assert_eq!(read_tmp.as_slice(), b"345678901234012\n");

		read_tmp.clear();
		assert!(read_buf.read_line(&mut read_tmp).is_err());
		assert_eq!(read_tmp.as_slice(), b"34");
	}
	#[test]
	fn test_read_buf_line_double() {
		let mut read_tmp = Vec::with_capacity(8);

		let mut read_buf = ReadBuf::new(16);
		let buf = read_buf.get_write_buf();
		assert_eq!(buf.len(), 16);
		buf[0..5].copy_from_slice(b"012\n\n");
		read_buf.write_len(5);
		read_buf.read_line(&mut read_tmp).expect("read bug");
		assert_eq!(read_tmp.as_slice(), b"012\n");
		read_tmp.clear();
		assert_eq!(read_buf.read_line(&mut read_tmp).unwrap(), 1);
		assert_eq!(read_tmp.as_slice(), b"\n");
	}

	#[test]
	fn test_read_buf_full() {
		let mut read_buf = ReadBuf::new(8);
		// 写入直到满
		let buf = read_buf.get_write_buf();
		assert_eq!(buf.len(), 8);
		buf.copy_from_slice(b"12345678");
		read_buf.write_len(8);
		assert_eq!(read_buf.available_space(), 0);
		// 再次获取写入缓冲区应该返回空切片
		let buf = read_buf.get_write_buf();
		assert_eq!(buf.len(), 0);
		// 读取一些数据
		let mut tmp = [0u8; 4];
		assert_eq!(read_buf.read(&mut tmp).unwrap(), 4);
		assert_eq!(&tmp, b"1234");
		// 现在可用空间应为4
		assert_eq!(read_buf.available_space(), 4);
		let buf = read_buf.get_write_buf();
		assert_eq!(buf.len(), 4);
		// 写入更多数据
		buf.copy_from_slice(b"abcd");
		read_buf.write_len(4);
		assert_eq!(read_buf.available_space(), 0);
		// 读取剩余数据
		let mut tmp = [0u8; 8];
		assert_eq!(read_buf.read(&mut tmp).unwrap(), 8);
		assert_eq!(&tmp, b"5678abcd");
	}

	#[test]
	fn test_read_buf_read_more_than_available() {
		let mut read_buf = ReadBuf::new(8);
		let buf = read_buf.get_write_buf();
		buf[..4].copy_from_slice(b"1234");
		read_buf.write_len(4);
		let mut tmp = [0u8; 10]; // 大于可用数据
		let read = read_buf.read(&mut tmp).unwrap();
		assert_eq!(read, 4);
		assert_eq!(&tmp[0..4], b"1234");
		// 缓冲区应为空
		assert_eq!(read_buf.is_empty(), true);
		// 再次读取应返回0
		assert_eq!(read_buf.read(&mut tmp).unwrap(), 0);
	}

	#[test]
	fn test_read_buf_write_more_than_space() {
		let mut read_buf = ReadBuf::new(8);
		let buf = read_buf.get_write_buf();
		assert_eq!(buf.len(), 8);
		// 写入超过可用空间的数据（Write trait）
		let written = read_buf.write(b"1234567890").unwrap();
		assert_eq!(written, 8); // 只写入8字节
		assert_eq!(read_buf.available_space(), 0);
		// 验证数据
		let mut tmp = [0u8; 8];
		assert_eq!(read_buf.read(&mut tmp).unwrap(), 8);
		assert_eq!(&tmp, b"12345678");
	}

	#[test]
	fn test_read_buf_line_with_existing_data() {
		let mut read_buf = ReadBuf::new(16);
		let buf = read_buf.get_write_buf();
		buf[..12].copy_from_slice(b"line1\nline2\n");
		read_buf.write_len(12);
		let mut output = Vec::new();
		output.extend_from_slice(b"prefix");
		assert_eq!(read_buf.read_line(&mut output).unwrap(), 12);
		assert_eq!(output.as_slice(), b"prefixline1\n");
		output.clear();
		output.extend_from_slice(b"prefix2");
		assert_eq!(read_buf.read_line(&mut output).unwrap(), 13);
		assert_eq!(output.as_slice(), b"prefix2line2\n");
	}

	#[test]
	fn test_read_buf_wraparound_multiple() {
		let mut read_buf = ReadBuf::new(8);
		let mut tmp = [0u8; 4];
		// 多次写入和读取，导致指针回绕多次
		for i in 0..10 {
			let buf = read_buf.get_write_buf();
			let to_write = std::cmp::min(buf.len(), 4);
			buf[..to_write].copy_from_slice(&[i; 4][..to_write]);
			read_buf.write_len(to_write);
			if to_write < 4 {
				let buf = read_buf.get_write_buf();
				buf[..4 - to_write].copy_from_slice(&[i; 4][to_write..]);
				read_buf.write_len(4 - to_write);
			}
			assert_eq!(read_buf.read(&mut tmp).unwrap(), 4);
			assert_eq!(&tmp, &[i; 4]);
		}
	}

	#[test]
	fn test_read_buf_capacity_one() {
		let mut read_buf = ReadBuf::new(1);
		assert_eq!(read_buf.capacity(), 1);
		assert_eq!(read_buf.available_space(), 1);
		let buf = read_buf.get_write_buf();
		assert_eq!(buf.len(), 1);
		buf[0] = b'A';
		read_buf.write_len(1);
		assert_eq!(read_buf.available_space(), 0);
		let mut tmp = [0u8; 1];
		assert_eq!(read_buf.read(&mut tmp).unwrap(), 1);
		assert_eq!(tmp[0], b'A');
		assert_eq!(read_buf.is_empty(), true);
		// 写入超过容量
		let written = read_buf.write(b"BC").unwrap();
		assert_eq!(written, 1);
		assert_eq!(read_buf.available_space(), 0);
		let mut tmp = [0u8; 1];
		assert_eq!(read_buf.read(&mut tmp).unwrap(), 1);
		assert_eq!(tmp[0], b'B');
	}

	#[test]
	fn test_read_buf_edge_cases() {
		// 测试空缓冲区多次读取返回0
		let mut read_buf = ReadBuf::new(8);
		let mut tmp = [0u8; 4];
		assert_eq!(read_buf.read(&mut tmp).unwrap(), 0);
		assert_eq!(read_buf.read(&mut tmp).unwrap(), 0);
		// 写入后立即读取
		let written = read_buf.write(b"xyz").unwrap();
		assert_eq!(written, 3);
		assert_eq!(read_buf.read(&mut tmp).unwrap(), 3);
		assert_eq!(&tmp[..3], b"xyz");
		// 缓冲区满时写入返回0
		let mut read_buf = ReadBuf::new(2);
		assert_eq!(read_buf.write(b"ab").unwrap(), 2);
		assert_eq!(read_buf.write(b"c").unwrap(), 0);
	}

}

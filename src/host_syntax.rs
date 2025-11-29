use regex::Regex;

/// Expand hostname syntax like "host<1-5>" into ["host1", "host2", "host3", "host4", "host5"]
pub fn expand_syntax(hostname: &str) -> Vec<String> {
	// Pattern: hostname<START-END> where START and END are numbers
	let re = Regex::new(r"^(.+)<(\d+)-(\d+)>$").unwrap();

	if let Some(caps) = re.captures(hostname) {
		let prefix = caps.get(1).unwrap().as_str();
		let start: u32 = caps.get(2).unwrap().as_str().parse().unwrap_or(0);
		let end: u32 = caps.get(3).unwrap().as_str().parse().unwrap_or(0);

		if start > end {
			return vec![hostname.to_string()];
		}

		let start_str = caps.get(2).unwrap().as_str();
		let num_zeros = start_str.len() - start_str.trim_start_matches('0').len();

		(start..=end)
			.map(|i| {
				let padded = if num_zeros > 0 {
					format!("{:0width$}", i, width = num_zeros + 1)
				} else {
					i.to_string()
				};
				format!("{prefix}{padded}")
			})
			.collect()
	} else {
		vec![hostname.to_string()]
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_expand_range() {
		let result = expand_syntax("host<1-3>");
		assert_eq!(result, vec!["host1", "host2", "host3"]);
	}

	#[test]
	fn test_expand_zero_padded() {
		let result = expand_syntax("host<01-03>");
		assert_eq!(result, vec!["host01", "host02", "host03"]);
	}

	#[test]
	fn test_no_expansion() {
		let result = expand_syntax("host1");
		assert_eq!(result, vec!["host1"]);
	}

	#[test]
	fn test_invalid_range() {
		let result = expand_syntax("host<5-1>");
		assert_eq!(result, vec!["host<5-1>"]);
	}
}

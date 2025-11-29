use std::collections::HashSet;

pub(crate) fn make_display_names(hosts: &[String]) -> (Vec<String>, usize) {
	let mut display_names = Vec::with_capacity(hosts.len());
	let mut used_names = HashSet::new();

	for host in hosts {
		if used_names.contains(host) {
			let mut i = 1;
			while used_names.contains(&format!("{}#{}", host, i)) {
				i += 1;
				continue;
			}
			let display_name = format!("{}#{}", host, i);
			display_names.push(display_name.clone());
			used_names.insert(display_name.clone());
		} else {
			display_names.push(host.clone());
			used_names.insert(host.clone());
		}
	}
	let max_len = display_names.iter().map(|x| x.len()).max().unwrap_or_default();
	(display_names, max_len)
}

#[cfg(test)]
mod tests {}

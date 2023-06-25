use std::{
  path::PathBuf,
  process::Command,
};
use crate::Error;
use xattr::set as xattr_set;
use log::info;
use apple_dmg::create_dmg;

pub fn make_dmg_in_linux(
  bundle_dir: &PathBuf,
  dmg_path: &PathBuf,
  bundle_file_name: &str,
  volname: &str,
  volicon: Option<&str>,
) -> crate::Result<()> {
  let mut file_list = vec!(
    (bundle_file_name, "/"),
  );

  if let Some(volicon_path) = volicon {
    file_list.push((volicon_path, ".VolumeIcon.icns"));
  }

  let mut total_size_bytes: u64 = 0;
  for (source_path, _) in &file_list {
    let file_bytes = get_file_bytes(bundle_dir, source_path)?;
    total_size_bytes += file_bytes + 20;
  }

  let total_sectors = (total_size_bytes as f64 / 512.0).ceil() as u32 + 100;

  info!("writing dmg with {} bytes ({} sectors) to {}", total_size_bytes, total_sectors, dmg_path.to_string_lossy());

  // set extended attribute on app dir
  // TODO: not working
  // let mut finder_info: [u8; 32] = [0; 32];
  // finder_info[8] = 4;
  // xattr_set(bundle_dir, "com.apple.FinderInfo", &finder_info)?;

  info!("set extended attribute on app dir");

  // create dmg file
  create_dmg(bundle_dir, dmg_path, volname, total_sectors)?;

  info!("dmg built");

  Ok(())
}

fn get_file_bytes(bundle_dir: &PathBuf, path: &str) -> crate::Result<u64> {
  // TODO: can this be done w/o invoking du? will just getting the file size have the same result?
  let du_output = Command::new("du")
    .current_dir(bundle_dir)
    .arg("-sb")
    .arg(path)
    .output()?;

  let du_output_str = String::from_utf8_lossy(&du_output.stdout);

  for part in du_output_str.split('\t') {
    if let Ok(n) = part.parse::<u64>() {
      return Ok(n);
    }
  }

  Err(Error::GenericError(format!("Failed to get bytes of {} using 'du' (no integer in du output: {})", path, du_output_str)))
}

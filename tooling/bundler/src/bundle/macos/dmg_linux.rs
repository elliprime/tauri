use std::{
  path::{Path, PathBuf},
  process::Command,
};
use crate::bundle::common::CommandExt;
use crate::Error;
use std::fs::{create_dir_all, rename};
use xattr::set as xattr_set;
use anyhow::Context;
use log::info;

pub fn make_dmg_in_linux(
  bundle_dir: &PathBuf,
  dmg_path: &PathBuf,
  dmg_name: &str,
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

  let mb = (1024 * 1024) as f64;
  let total_size_in_mb = (total_size_bytes as f64 / mb).ceil();

  let dmg_path_tmp = format!("{}.dmg", bundle_file_name);

  info!("creating dmg with size of {}M", total_size_in_mb);

  // create dmg file
  Command::new("dd")
    .current_dir(bundle_dir)
    .arg("if=/dev/zero")
    .arg(format!("of={}", &dmg_path_tmp))
    .arg("bs=1M")
    .arg(format!("count={}", total_size_in_mb))
    .arg(format!("status=progress"))
    .output_ok()
    .context("error running dd to create dmg file")?;

  Command::new("mkfs.hfsplus")
    .current_dir(bundle_dir)
    .arg("-v")
    .arg(volname)
    .arg(&dmg_path_tmp)
    .output_ok()
    .context("error running mkfs.hfsplus")?;

  // mount dmg in a temp folder
  let mount_path = format!("{}_tmp", &dmg_path_tmp);
  create_dir_all(Path::new(bundle_dir).join(&mount_path))?;

  // creating a loopback device in docker requires access to /dev which requires changing to the root user which requires sudo
  // TODO: maybe just switch to pkg files
  Command::new("sudo")
    .current_dir(bundle_dir)
    .args(["mount", "-t", "hfsplus", "-o", "loop", &dmg_path_tmp, mount_path.as_str()])
    .output_ok()
    .context("failed to mount dmg file")?;

  // copy files over
  let mut copy_failed = false;
  for (source_path, relative_target_path) in &file_list {
    let result = Command::new("cp")
      .args(["-r", source_path, format!("{}/{}", &mount_path, relative_target_path).as_str()])
      .output_ok();
    if result.is_err() {
      copy_failed = true;
      break;
    }
  }

  if copy_failed {
    unmount(mount_path.as_str())?;
    return Err(Error::GenericError(String::from("failed to copy files into mounted dmg")));
  }

  // set extended attribute on mount dir
  let mut finder_info: [u8; 32] = [0; 32];
  finder_info[8] = 4;
  xattr_set(mount_path.as_str(), "com.apple.FinderInfo", &finder_info)?;

  unmount(mount_path.as_str())?;

  rename(bundle_dir.join(dmg_name), dmg_path.clone())?;

  Ok(())
}

fn get_file_bytes(bundle_dir: &PathBuf, path: &str) -> crate::Result<u64> {
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

fn unmount(path: &str) -> crate::Result<()> {
  Command::new("umount").arg(path).output_ok()?;
  Ok(())
}

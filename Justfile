@_default:
  @just --list

# vendor dependencies and create tarball
vendor:
  rm -rf .cargo vendor vendor.tar.gz
  mkdir .cargo
  cargo vendor > .cargo/config
  tar -czvf vendor.tar.gz .cargo vendor
  rm -rf .cargo vendor

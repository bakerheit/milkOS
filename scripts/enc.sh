# Sourced by test scripts: MILK_ENCRYPT=1 runs them on encrypted milkfs volumes.
#   milk_enc_setup DIR  -> ENC_MKFS (extra mkmilkfs.py mkfs args), ENC_QEMU (extra qemu
#                          args: the test passphrase via fw_cfg, so boots unlock
#                          unattended), and MILKFS_PASSPHRASE for the Mac-side tools.
# The passphrase is a test value, obviously fake. Use ${ENC_MKFS[@]+"${ENC_MKFS[@]}"}.
milk_enc_setup() {
  ENC_MKFS=()
  ENC_QEMU=()
  if [ "${MILK_ENCRYPT:-0}" = 1 ]; then
    export MILKFS_PASSPHRASE="not-a-real-passphrase"
    export MILKFS_NEW_PASSPHRASE="$MILKFS_PASSPHRASE"
    ENC_MKFS=(--encrypt --recovery-out "$1/recovery-key")
    ENC_QEMU=(-fw_cfg "name=opt/milkos/passphrase,string=$MILKFS_PASSPHRASE")
    echo "(encrypted volumes, test passphrase)"
  fi
}

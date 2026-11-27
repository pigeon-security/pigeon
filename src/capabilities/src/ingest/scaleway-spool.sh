# The Scaleway SBS guest device advertises the serial "volume-<provider-id>".
# Require the exact attachment observed in the create response and fail closed
# when the guest does not expose a matching blank block device.
mount_spool() {
    local volume_id="$1"
    local expected_bytes="$2"
    local device="/dev/disk/by-id/scsi-0SCW_sbs_volume-$volume_id"
    local root_disk
    local disk
    local attempt
    local signatures

    root_disk="$(findmnt -n -o SOURCE /)"
    root_disk="$(lsblk -s -nrpo NAME,TYPE "$root_disk" | awk '$2 == "disk" { print $1 }')"
    [[ -b "$root_disk" ]] || return 1

    for ((attempt = 0; attempt < 30; attempt++)); do
        [[ -b "$device" ]] && break
        sleep 2
    done
    [[ -b "$device" ]] || return 1
    disk="$(readlink -f "$device")"
    [[ "$disk" != "$root_disk" ]] || return 1
    [[ "$(lsblk -dnro TYPE,SIZE -b "$disk")" == "disk $expected_bytes" ]] || return 1
    [[ "$(lsblk -nrpo NAME "$disk")" == "$disk" ]] || return 1
    if findmnt -n -S "$disk" >/dev/null; then
        return 1
    fi
    signatures="$(wipefs --noheadings --output TYPE "$disk")" || return 1
    [[ -z "$signatures" ]] || return 1

    mkfs.ext4 -q -m 0 "$disk"
    install -d -m 0700 /spool
    mount -o nodev,nosuid,noexec "$disk" /spool
    chown 10001:10001 /spool
    chmod 0700 /spool
}

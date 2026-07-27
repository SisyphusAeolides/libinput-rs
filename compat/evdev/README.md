# libinput-rs-evdev

`libinput-rs-evdev` re-exports `evdev` 0.13.2 and replaces only device enumeration.

The compatibility enumerator lists `/dev/input/event*` directory entries without opening them. This lets callers discover devices first and delegate access to a privileged `open_restricted` callback, as compositors using logind require.

All other public types and behavior come from the upstream `evdev` crate.

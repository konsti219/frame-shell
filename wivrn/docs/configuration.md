# Configurable items

Configuration is split between headset and server. Headset contains most configuration, while server has system specific items such as encoders and paths.
Files are read from
- `/usr/share/wivrn/config.json` (where `/usr` is selected at configure time with `CMAKE_INSTALL_PREFIX`)
- `/etc/wivrn/config.json`
- `$XDG_CONFIG_HOME/wivrn/config.json` or if `$XDG_CONFIG_HOME` is not set, `$HOME/.config/wivrn/config.json`.

Files later in the list replace top-level values from previous ones.

If you installed WiVRn from a flatpack, the config is in `$HOME/.var/app/io.github.wivrn.wivrn/config/wivrn/config.json`.

All elements are optional and have default values.

## `bit-depth`
Default value: 10-bit if supported by the selected codec, encoder and decoder. 8-bit otherwise.

Bit depth of the video. 8-bit is supported by all encoders. 10-bit is supported by `vulkan`, `vaapi` and `nvenc` with `h265` or `av1` codecs.

## `encoder`
The encoder to use, either a single string or object applied to all streams, or a list of string or objects with values for left, right and alpha.
When a string it is used, it is equivalent to the `encoder` item of the object.

WiVRn encodes each eye separately, and the alpha channel as one for both eyes. Each stream is processed independently, this may use resources more effectively and reduce latency.

### `encoder`
Default value, in order: `vulkan` if supported, `nvenc` on Nvidia GPU, `vaapi` on all other GPU, else `x264`.

Identifier of the encoder, one of
* `x264`: software encoding
* `nvenc`: Nvidia hardware encoding
* `vaapi`: AMD/Intel hardware encoding
* `vulkan`: Vulkan Video encode, cross-vendor hardware encoding

### `codec`
Default value: best supported by both headset and encoder of `av1`, `h264`, `h265`.

One of `h264`, `h265`, `av1`, `raw`.

Not all encoders support every codec:
- `x264` encoder only supports `h264` codec
- `vulkan` encoder supports `h264` and `h265` codecs
- `raw` encoder only supports `raw` codec
- `nvenc` and `vaapi` support all codecs, except `raw`

If `nvenc` encoder is in use, you can refer to [nvidia website](https://developer.nvidia.com/video-encode-decode-support-matrix) to make sure that your GPU supports encoding with the desired codec.

### Examples
1. Simple configuration
```json
{
	"encoder": {
		"encoder": "vaapi",
		"codec": "h265"
	}
}
```
Use vaapi hardware encoding, h265 video codec (HEVC).

2. Hardware + software encoder
```json
{
	"encoder": [
		{
			"encoder": "vaapi",
			"codec": "h265",
		},
		{
			"encoder": "x264",
			"codec": "h264",
		},
		{
			"encoder": "vaapi",
			"codec": "h265",
		},
	]
}
```
Creates a hardware encoder for left eye and transparency, and a software encoder for right eye.

### `device`, only for vaapi
Default value: unset

Manually specify the device for encoding, can be used to offload encode to an iGPU. Device shall be in the form "/dev/dri/renderD128".


### `options` (very advanced), only for vaapi
Default value: unset

Json object of additional options to pass directly to ffmpeg `avcodec_open2`'s `option` parameter.

For vulkan, `buffer_ms` (a string, 10 to 1000, default `"1000"`) sets the rate control window: a shorter one keeps single frames closer to the average size, so large frames don't arrive as bursts the network drops.

## `application`
Default value: unset

An application to start when connection with the headset is established, can be a string or an array of strings if parameters need to be provided.

### Example
```json
{
	"application": ["steam", "steam://launch/275850/VR"]
}
```
Launch No Man's Sky in VR mode on Steam when connection with headset is established.

## Multiple network links

With `tcp-only` disabled, WiVRn automatically tests paths over the available IP interfaces.
After pairing, the client and server exchange endpoints and validate paths in
both directions. Usable paths share one streaming session. IPv4 and IPv6
addresses between the same client and server interfaces are alternatives for
one link.

Video traffic is distributed using per-path capacity estimates and frame
deadlines. Tracking and audio take priority over queued video. Control messages
are delivered reliably and in order over surviving paths. Missing video packets
can be repaired over another path when time remains before their deadline.

The encoder bitrate decreases when the estimated capacity cannot sustain the
requested rate and rises back toward it as capacity recovers. The requested
bitrate remains the target. Links can share a radio or another bottleneck, so adding interfaces does not
guarantee additional capacity. Build both client and server from the same
revision. Interfaces must be exposed to the application by the operating
system. `tcp-only` retains a single TCP connection; ADB TCP forwarding is not
an additional UDP path. For USB to participate in multi-link streaming, it must
provide an IP network interface.

The client's `multilink_enabled` setting turns multi-link on or off for new
connections. `link_mode` selects the links used while streaming: `preferred`
uses the preferred link while it works and every link otherwise, `all` uses
every link and `fastest` keeps only the best measured path. The client's
`preferred_interface` setting names the preferred link's interface, for example
`wlan1`. When it is empty there is no preferred link, and `preferred` behaves
like `all`.

### `multilink-fec`

Default value: `false`. On a link with 0.5% to 10% loss and spare capacity, adds
one XOR parity packet to each group of four video packets, so a single lost
packet in a group is rebuilt without a resend.

### `multilink-shared-bottleneck`

Default value: `false`. Set to `true` when the links share a radio or another
bottleneck. The combined send rate is then lowered to what the links deliver
together when they start losing packets.

### `multilink-rate-limit`

Default value: `0`. Limits the combined send rate over all links, in bits per
second. `0` removes the limit. The value must not exceed `1000000000`.

## `tcp-only`
Default value: `false`

Only use TCP for communications with the client, this may have increased latency.
If `false` or unset, WiVRn will use both TCP and UDP.
Setting this to `true` disables [multi-link streaming](#multiple-network-links).

### Example
```json
{
	"tcp-only": true
}
```

## `publish-service`
Default value: `avahi`

How to publish the service over the network, `avahi` or null.

If set to null, service will not be published and address has to be entered manually on the headset.

## `openvr-compat-path`
Default value: unset

Provides the path to the directory of an OpenVR compatibility tool (such as OpenComposite).

If unset, WiVRn will autodetect the path of such a tool as usual (see [the SteamVR guide](./steamvr.md)).

If set to an null, WiVRn will not manage the OpenVR configuration.

## `hid-forwarding`
Default value: `false`

Only available when the `uinput` kernel module is loaded and the user has write access.

Mirrors input devices forwarded from the headset (keyboard, mouse, gamepad) to `uinput`
devices, for applications that do not read them through OpenXR. Which devices are forwarded is
chosen on the headset.

A forwarded gamepad is also exposed as a native OpenXR gamepad at `/user/gamepad`, which needs
no permission and is always available. Only the digested controller state is forwarded
(buttons, two sticks, analog triggers and a d-pad), so device specific features such as gyro,
adaptive triggers or the touchpad are not available.

## `debug-gui`
Default value: `false`

Only available when built with `WIVRN_FEATURE_DEBUG_GUI`.

Enables the Monado debug gui.

## `use-steamvr-lh`
Default value: `false`

Only available when built with `WIVRN_FEATURE_STEAMVR_LIGHTHOUSE`.

Enables the driver to load SteamVR Lighthouse devices.

## `lh-max-extrapolation`
Default value: unset

Only available when built with `WIVRN_FEATURE_STEAMVR_LIGHTHOUSE`.

Maximum time in milliseconds that poses may be extrapolated ahead for SteamVR Lighthouse devices.

## `lh-stick-deadzone`
Default value: `0`

Only available when built with `WIVRN_FEATURE_STEAMVR_LIGHTHOUSE`

Applies a deadzone to joysticks on SteamVR controllers (e.g. Index).

## `port`
Default value: `9757`

Change the TCP/UDP port used for the connection.

## `hostname`
Default value: unset

If set, overrides the name displayed in the server list.

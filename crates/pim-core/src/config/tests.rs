mod bluetooth;
mod discovery;
mod general;
mod peers;
mod relay;
mod security;
mod transport;
mod wifi_direct;

const MINIMAL_CONFIG: &str = r#"
[node]
name = "test-node"
"#;

const FULL_CONFIG: &str = r#"
[node]
name = "my-device"
data_dir = "/tmp/pim"

[interface]
name = "pim0"
mtu = 1400
mesh_ip = "auto"
mesh_ipv6 = "fd77::10/64"

[discovery]
enabled = true
port = 9101
broadcast_interval_ms = 5000
peer_timeout_ms = 30000
connect_relays = true
connect_gateways = true
shared_key = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"

[relay]
enabled = false

[transport]
type = "tcp"
listen_port = 9100
max_reconnect_attempts = 20
connect_timeout_ms = 3000

[routing]
max_hops = 10
algorithm = "distance-vector"
route_expiry_s = 300

[gateway]
enabled = true
nat_interface = "wlan0"
max_connections = 200

[security]
key_file = "/tmp/pim/node.key"
require_encryption = true
authorization_policy = "allow_list"
authorized_peers = ["abababababababababababababababab"]
trust_store_file = "/tmp/pim/trusted-peers.toml"

[wifi_direct]
enabled = false
interface = "wlan0"
go_intent = 7
listen_channel = 6
op_channel = 6
connect_method = "pbc"

[bluetooth]
enabled = false
interface = "auto"
radio_discovery_enabled = true
device_name_prefix = "PIM-"
local_alias = ""
connect_pan = true
serve_nap = false
nap_bridge = "br-bt"
auto_discover_peers = true
poll_interval_ms = 2000
scan_interval_ms = 5000
peer_discovery_interval_ms = 2000
bluetoothctl_timeout_s = 15
discoverable_timeout_s = 180
startup_timeout_ms = 15000

[bluetooth_rfcomm]
enabled = false
channel = 22
device_name_prefix = "PIM-"
outbound_enabled = true
poll_interval_ms = 30000
bridge_to_tcp = true
"#;

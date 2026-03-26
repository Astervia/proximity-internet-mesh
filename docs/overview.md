# 🚀 Project Overview — _Proximity Internet Mesh_

## 🧠 Summary

> **Proximity Internet Mesh** is a decentralized networking system that enables devices to access the internet indirectly through nearby peers using a multi-hop, peer-to-peer architecture.

Instead of connecting directly to the internet, devices join a **local proximity mesh**, where requests are routed across nearby nodes until reaching a device with internet connectivity (gateway node), which performs the request and returns the response through the mesh.

The system leverages:

- **Wi-Fi Direct for high-bandwidth data transfer**
- Bluetooth Low Energy (optional) for discovery and coordination
- A lightweight custom protocol for routing and request handling

# 🎯 Goals

## 1. 🌐 Enable Indirect Internet Access

Allow devices without connectivity to:

- Perform HTTP/API requests
- Fetch remote resources
- Communicate with cloud services

…through nearby peers with internet access.

## 2. 🔗 Build a Decentralized Mesh Network

- No central infrastructure
- Peer-to-peer routing
- Multi-hop communication between devices

## 3. ⚡ Maximize Throughput and Efficiency

- Use Wi-Fi Direct for data plane
- Minimize BLE usage (control only)
- Optimize payload size and routing

## 4. 🔐 Ensure Security and Trust

- End-to-end encryption
- Node identity via public/private keys
- Protection against malicious gateways

## 5. 📱 Operate on Commodity Mobile Devices

- Android-first implementation
- No special hardware required
- Works in:
    - offline environments
    - dense urban areas
    - disaster scenarios

# 🧱 Core Architecture

## 🔍 Discovery Layer

- Detect nearby devices
- Exchange capabilities:
    - “I am a gateway”
    - supported transports

**Technologies:**

- BLE (optional)
- Wi-Fi Aware (future)

## 🔗 Control Layer

- Establish connections
- Negotiate transport (Wi-Fi Direct)
- Manage routing metadata

## 🚀 Data Layer (Primary)

- High-bandwidth communication via:
    - **Wi-Fi Direct**

- Handles:
    - request forwarding
    - response streaming
    - chunked transfers

## 🌐 Gateway Layer

Nodes with internet access:

- Receive routed requests
- Execute HTTP calls
- Return responses to origin node

## 📦 Application Layer

Client-facing interface:

- Mobile app / SDK
- Abstracts mesh complexity
- Exposes:
    - HTTP-like API
    - request/response model

# 🔄 Request Lifecycle

```text
Client Device
   ↓
Local Peer (Wi-Fi Direct)
   ↓
Intermediate Nodes (multi-hop)
   ↓
Gateway Node (internet access)
   ↓
External API
   ↓
Response returns via same path
```

# ⚙️ Key Design Principles

## 🧠 1. Transport Adaptation

- Always prefer:
    1. Wi-Fi Direct
    2. (fallbacks later)

## 📉 2. Minimize Overhead

- Avoid raw HTTP when possible
- Use compact binary protocol (e.g. protobuf)

## 🔁 3. Store-and-Forward

- Nodes can buffer messages
- Enables intermittent connectivity

## 🧩 4. Modular Networking Stack

- Decouple:
    - transport
    - routing
    - application logic

## 🔐 5. Security by Default

- Encrypted payloads
- Signed messages
- Optional onion routing (future)

# 🚧 Non-Goals (for MVP clarity)

To stay focused:

- ❌ No full internet browsing (initially)
- ❌ No streaming / large file transfer
- ❌ No global-scale routing optimization
- ❌ No incentive/token system (yet)

# 🧪 MVP Scope

## Phase 1 — Direct Proxy

- 2 devices:
    - Client
    - Gateway

- Wi-Fi Direct connection
- Forward HTTP requests

## Phase 2 — Multi-Hop

- Add relay nodes
- Basic forwarding logic
- No advanced routing yet

## Phase 3 — Mesh Intelligence

- Gateway discovery
- Routing optimization
- Multiple gateways

# 🌍 Use Cases

- 📡 Connectivity in low-infrastructure regions
- 🚨 Disaster recovery (no internet available)
- 🪖 Tactical / field communication
- 🎉 Dense environments (events, campuses)
- 🔒 Privacy-focused networking

# 🔥 Positioning (This is strong)

> A **decentralized, proximity-based networking layer** that transforms nearby devices into a collaborative internet access mesh.

# 🧠 Final Insight

By choosing **Wi-Fi Direct as the data plane**, your project becomes:

> ❌ Not constrained by BLE
> ✅ A real distributed networking system with mobile nodes

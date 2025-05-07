### Core Messages

**1. Register Device**

*   **Direction:** Client → Server
*   **Purpose:** Register with a hardware ID hash for persistent identification

```json
{
  "type": "Register",
  "payload": {
    "hardware_id": "71F7EE6758F98D5DA3FBA1D08E577D8CE2E0B0362A6F29DF691B2E5E5D6922A3"
  }
}
```

**2. Registration Response**

*   **Direction:** Server -> Client
*   **Purpose:** Provide the unique `device_id` for this session

```json
{
  "type": "RegisterResponse",
  "payload": {
    "device_id": "d8f8a9b0-f6e1-4a2b-8d7c-1e9f0a1b2c3d"
  }
}
```

**3. Send Message**

*   **Direction:** Client -> Server
*   **Purpose:** Request the server relay a message to another device

```json
{
  "type": "Send",
  "payload": {
    "recipient_id": "2903000783872D8AAB596EB0B042E651C7C2845BB641433DFAC0C3F9392C13F4",
    "sender_id": "71F7EE6758F98D5DA3FBA1D08E577D8CE2E0B0362A6F29DF691B2E5E5D6922A3",
    "encrypted_data": "Base64EncodedEncryptedBlob==",
    "message_id": "f47ac10b-58cc-4372-a567-0e02b2c3d479",
    "timestamp": 1714608157,
    "ttl": 300,
    "signature": "OptionalBase64EncodedSignature==",
    "hardware_id": "71F7EE6758F98D5DA3FBA1D08E577D8CE2E0B0362A6F29DF691B2E5E5D6922A3",
    "content_type": "Text",
    "size_bytes": 12345
  }
}
```

**4. Receive Message**

*   **Direction:** Server -> Client
*   **Purpose:** Deliver a message from another device to the recipient. (Same as Send Message)

```json
{
  "type": "Receive",
  "payload": {
    "recipient_id": "2903000783872D8AAB596EB0B042E651C7C2845BB641433DFAC0C3F9392C13F4",
    "sender_id": "71F7EE6758F98D5DA3FBA1D08E577D8CE2E0B0362A6F29DF691B2E5E5D6922A3",
    "encrypted_data": "Base64EncodedEncryptedBlob==",
    "message_id": "f47ac10b-58cc-4372-a567-0e02b2c3d479",
    "timestamp": 1714608157,
    "ttl": 300,
    "signature": "OptionalBase64EncodedSignature==",
    "hardware_id": "71F7EE6758F98D5DA3FBA1D08E577D8CE2E0B0362A6F29DF691B2E5E5D6922A3",
    "content_type": "Text",
    "size_bytes": 12345
  }
}
```

**5. Acknowledge Message**

*   **Direction:** Bidirectional (Client -> Server or Server -> Client)
*   **Purpose:** Confirm message delivery or queuing.

```json
{
  "type": "Ack",
  "payload": {
    "message_id": "f47ac10b-58cc-4372-a567-0e02b2c3d479"
  }
}
```

**6. Ping / Pong**

*   **Direction:** Bidirectional (Client -> Server or Server -> Client)
*   **Purpose:** Keep connection alive

```json
{
  "type": "Ping"
}
```
```json
{
  "type": "Pong"
}
```

**7. Error**

*   **Direction:** Server -> Client
*   **Purpose:** Notify the client about errors or special conditions

```json
{
  "type": "Error",
  "payload": {
    "message": "Rate limit exceeded (60/60). Try again in a minute.",
    "code": 429
  }
}
```

---

### Authentication Messages

**1. Authentication Request**

*   **Direction:** Client -> Server -> Client
*   **Purpose:** Initiate the challenge-response authentication

```json
{
  "type": "AuthRequest",
  "payload": {
    "device_id": "71F7EE6758F98D5DA3FBA1D08E577D8CE2E0B0362A6F29DF691B2E5E5D6922A3",
    "public_key": "Base64PublicKey==",
    "challenge": "RandomChallengeString",
    "hardware_id": "71F7EE6758F98D5DA3FBA1D08E577D8CE2E0B0362A6F29DF691B2E5E5D6922A3"
  }
}
```

**2. Authentication Response**

*   **Direction:** Client -> Server -> Client
*   **Purpose:** Respond to challenge and issue counter-challenge

```json
{
  "type": "AuthResponse",
  "payload": {
    "device_id": "2903000783872D8AAB596EB0B042E651C7C2845BB641433DFAC0C3F9392C13F4",
    "challenge_response": "Base64SignatureOfReceivedChallenge==",
    "challenge": "NewChallengeForOriginalRequester",
    "hardware_id": "2903000783872D8AAB596EB0B042E651C7C2845BB641433DFAC0C3F9392C13F4"
  }
}
```

**3. Authentication Verification**

*   **Direction:** Client -> Server -> Client
*   **Purpose:** Respond to the challenge received in `AuthResponse`.

```json
{
  "type": "AuthVerify",
  "payload": {
    "device_id": "d8f8a9b0-f6e1-4a2b-8d7c-1e9f0a1b2c3d", // Original Sender's ID
    "challenge_response": "Base64SignatureOfRespondersChallenge==",
    "hardware_id": "71F7EE6758F98D5DA3FBA1D08E577D8CE2E0B0362A6F29DF691B2E5E5D6922A3"
  }
}
```

**4. Authentication Success**

*   **Direction:** Client -> Server -> Client (Potentially)
*   **Purpose:** Optionally confirm successful mutual verification.

```json
{
  "type": "AuthSuccess",
  "payload": {
    "device_id": "a1b2c3d4-e5f6-7890-1234-567890abcdef", // ID of the device being confirmed as trusted
    "trusted": true,
    "hardware_id": "1E0D2B3C4A5F8E9D6B0C1F2A3D5E6C8E3917DEF0B65C6A143F7E209D8CB7A5"
  }
}
```

---

### Key Rotation Messages (Facilitated by Server)

*(Used for secure updates of end-to-end encryption keys)*

**1. Key Rotation Update**

*   **Direction:** Client -> Server -> Client
*   **Purpose:** Send new encrypted key material to a peer.

```json
{
  "type": "KeyRotationUpdate",
  "payload": {
    "sender_id": "d8f8a9b0-f6e1-4a2b-8d7c-1e9f0a1b2c3d",
    "recipient_id": "a1b2c3d4-e5f6-7890-1234-567890abcdef",
    "encrypted_key_package": "Base64EncryptedKeyData==",
    "key_id": 1678886401, // Example: Timestamp or sequence number
    "hardware_id": "71F7EE6758F98D5DA3FBA1D08E577D8CE2E0B0362A6F29DF691B2E5E5D6922A3"
  }
}
```

**2. Key Rotation Acknowledgment**

*   **Direction:** Client -> Server -> Client
*   **Purpose:** Confirm receipt and processing of a key update.

```json
{
  "type": "KeyRotationAck",
  "payload": {
    "sender_id": "a1b2c3d4-e5f6-7890-1234-567890abcdef", // ID of device sending the Ack
    "recipient_id": "d8f8a9b0-f6e1-4a2b-8d7c-1e9f0a1b2c3d", // ID of original key sender
    "key_id": 1678886401,
    "success": true, // Or false if processing failed
    "hardware_id": "1E0D2B3C4A5F8E9D6B0C1F2A3D5E6C8E3917DEF0B65C6A143F7E209D8CB7A5"
  }
}
```

---

### Status Message

**1. Update Status**

*   **Direction:** Client -> Server
*   **Purpose:** Inform the server of the client's presence status.

```json
{
  "type": "Status",
  "payload": {
    "device_id": "d8f8a9b0-f6e1-4a2b-8d7c-1e9f0a1b2c3d",
    "status": "Online", // Or "Away", "Offline"
    "hardware_id": "71F7EE6758F98D5DA3FBA1D08E577D8CE2E0B0362A6F29DF691B2E5E5D6922A3"
  }
}
```
---
### Core Messages

**1. Register Device**

*   **Direction:** Client -> Server
*   **Purpose:** Request a unique ID for this connection session.

```json
{
  "type": "Register"
}
```

**2. Registration Response**

*   **Direction:** Server -> Client
*   **Purpose:** Provide the unique `device_id` for this session.

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
*   **Purpose:** Request the server relay a message to another device.

```json
{
  "type": "Send",
  "payload": {
    "recipient_id": "a1b2c3d4-e5f6-7890-1234-567890abcdef",
    "sender_id": "d8f8a9b0-f6e1-4a2b-8d7c-1e9f0a1b2c3d",
    "encrypted_data": "Base64EncodedEncryptedBlob==",
    "message_id": "f47ac10b-58cc-4372-a567-0e02b2c3d479",
    "timestamp": 1678886400,
    "ttl": 300,
    "signature": "OptionalBase64EncodedSignature==",
    "content_type": "Text", // or "Image", "KeyPackage", "Other"
    "size_bytes": 12345
  }
}
```

**4. Receive Message**

*   **Direction:** Server -> Client
*   **Purpose:** Deliver a relayed message from another device. (Payload structure matches `Send`)

```json
{
  "type": "Receive",
  "payload": {
    "recipient_id": "d8f8a9b0-f6e1-4a2b-8d7c-1e9f0a1b2c3d",
    "sender_id": "a1b2c3d4-e5f6-7890-1234-567890abcdef",
    "encrypted_data": "Base64EncodedEncryptedBlob==",
    "message_id": "f47ac10b-58cc-4372-a567-0e02b2c3d479",
    "timestamp": 1678886400,
    "ttl": 300,
    "signature": "OptionalBase64EncodedSignature==",
    "content_type": "Text",
    "size_bytes": 12345
  }
}
```

**5. Acknowledge Message**

*   **Direction:** Server -> Client (Typically) or Client -> Server (Less Common)
*   **Purpose:** Confirm message delivery or processing.

```json
{
  "type": "Ack",
  "payload": {
    "message_id": "f47ac10b-58cc-4372-a567-0e02b2c3d479"
  }
}
```

**6. Ping**

*   **Direction:** Client -> Server or Server -> Client
*   **Purpose:** Check connection health / keep-alive.

```json
{
  "type": "Ping"
}
```

**7. Pong**

*   **Direction:** Server -> Client or Client -> Server
*   **Purpose:** Response to a `Ping`.

```json
{
  "type": "Pong"
}
```

**8. Error Notification**

*   **Direction:** Server -> Client
*   **Purpose:** Inform the client about an error.

```json
{
  "type": "Error",
  "payload": {
    "message": "Rate limit exceeded (60/60). Try again in a minute.",
    "code": 429 // Optional status code
  }
}
```

---

### Authentication Messages (Facilitated by Server)

*(These messages enable devices to verify each other directly)*

**1. Authentication Request**

*   **Direction:** Client -> Server -> Client
*   **Purpose:** Initiate challenge to verify recipient's identity.

```json
{
  "type": "AuthRequest",
  "payload": {
    "device_id": "d8f8a9b0-f6e1-4a2b-8d7c-1e9f0a1b2c3d", // Sender's ID
    "public_key": "Base64PublicKey==",
    "challenge": "RandomChallengeString"
  }
}
```

**2. Authentication Response**

*   **Direction:** Client -> Server -> Client
*   **Purpose:** Respond to sender's challenge, issue own challenge.

```json
{
  "type": "AuthResponse",
  "payload": {
    "device_id": "a1b2c3d4-e5f6-7890-1234-567890abcdef", // Responder's ID
    "challenge_response": "Base64SignatureOfSendersChallenge==",
    "challenge": "NewRandomChallengeStringForSender"
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
    "challenge_response": "Base64SignatureOfRespondersChallenge=="
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
    "trusted": true
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
    "key_id": 1678886401 // Example: Timestamp or sequence number
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
    "success": true // Or false if processing failed
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
    "status": "Online" // Or "Away", "Offline"
  }
}
```
---
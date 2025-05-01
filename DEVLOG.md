# Development Log - OpenRelay Server

This log tracks the development progress, major changes, decisions, and bug fixes for the OpenRelay server project.

**Maintainer:** Awe03
**Repository:** https://github.com/OpenSystemsDev/OpenRelay-Server

---

## 2025-04-23

**Time:** 16:27:05 UTC | **Author:** Awe03

### Implemented Core Features (Rate Limit, Bandwidth, Queuing, Cleanup)

*   **Added:**  A `dual rate limiting mechanism`, instead of the previous 60 messages per minute, 10kb max text size, 10mb max image size.  
                Having a point-based system allows for more flexibility. The user may require to send one large text / image. Via this method, it will be allowed (of course as long as its smaller than the point limit), at the expense of them being rate limited for the remaining minute. This way, users can send multiple small messages or a singular large message, depending on their needs.  
                The 60 messages per minute is necessary to prevent users from flooding the server with small messages.  
                Handling limits over different file types is easier now. Dont need to explicitly set max sizes for each file type to ensure it stays within feasible limits.
*   **Added:**  `Global monthly bandwidth tracking` to stay within oracle cloud free tier limits (10tb). If usage nears it, the server starts rejecting every request to ensure 
                that I dont exceed the limit.
*   **Documented:**  Created `README.md`
*   **Documented:**  Created `RESPONSES.md` containing all messages sent/received by the server, kind of like an API documentation. 
*   **Decision:**  Rate limiting is connection-based (not IP and device id) for privacy. This leaves a potential vunerability for a user to quit and reopen OpenRelay to bypass the
                rate limit. In favour of privacy, I have decided to take this risk, and analyse usage patterns to see if this is a problem. If it is, I will implement a more complex rate limiting system. Maybe client side prevention mechanisms.
                I should probably address this tomorrow.
---

## 2025-04-25

**Time:** 11:32:13 UTC | **Author:** Awe03  

### Updated Docs, Use a hash of the user's hardware IDs for identification and enforcement of rate limiting, Proper implementation of the pairing system
* **Decision:** Using a hash of the user's hardware IDs (CPU, Memory, Disk and the Hardware ID) instead of a temporary WebSocket id ensures that the user cannot bypass rate limits by reconnecting, and allows persistent pairing over client restarts.
* **Updated:** README.md now explains what the user's hardware has is used for, and how it is used.

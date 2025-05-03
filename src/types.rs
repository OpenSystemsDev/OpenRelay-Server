use serde::{ Deserialize, Serialize };
use std::time::{ Duration, Instant };
use std::sync::atomic::{ AtomicU64, Ordering };
use std::fs::File;
use std::io::{ Read, Write };
use std::path::Path;
use uuid::Uuid;
use dashmap::DashMap;
use chrono::{ Utc, Datelike };
use tracing::{ error, info };

// Store message queues for offline devices
pub type MessageQueues = DashMap<String, Vec<QueuedMessage>>;

// Store last activity time for each device
pub type LastActive = DashMap<String, Instant>;

// Store hardware ID to device ID mappings
pub type HardwareIdMap = DashMap<String, String>;

// Store device ID to hardware ID mappings (reverse lookup)
pub type DeviceIdToHwIdMap = DashMap<String, String>;

// Store rate limits by hardware ID
pub type HardwareRateLimits = DashMap<String, RateLimit>;

// Store active connections by device ID
pub type ActiveConnections = DashMap<String, String>; // device_id -> connection_id

// Create a struct to track monthly bandwidth usage
pub struct BandwidthTracker {
    // Current month's data transfer in bytes
    monthly_bytes: AtomicU64,
    // The month we're tracking (1-12)
    current_month: AtomicU64,
    // The year we're tracking
    current_year: AtomicU64,
    // Monthly limit in bytes (10TB = 10 trillion bytes)
    monthly_limit: u64,
    // Warning threshold (9.5TB = 9.5 trillion bytes)
    warning_threshold: u64,
    // Path to store bandwidth data
    storage_path: String,
}

// Manual implementation of Clone for BandwidthTracker
impl Clone for BandwidthTracker {
    fn clone(&self) -> Self {
        Self {
            monthly_bytes: AtomicU64::new(self.monthly_bytes.load(Ordering::Relaxed)),
            current_month: AtomicU64::new(self.current_month.load(Ordering::Relaxed)),
            current_year: AtomicU64::new(self.current_year.load(Ordering::Relaxed)),
            monthly_limit: self.monthly_limit,
            warning_threshold: self.warning_threshold,
            storage_path: self.storage_path.clone(),
        }
    }
}

impl BandwidthTracker {
    pub fn new(storage_path: &str) -> Self {
        // Set limits - 10TB and 9.5TB in bytes
        let monthly_limit = 10_000_000_000_000;
        let warning_threshold = 9_500_000_000_000;
        
        // Get current month and year
        let now = Utc::now();
        let month = now.month() as u64;
        let year = now.year() as u64;
        
        // Create initial tracker
        let mut tracker = Self {
            monthly_bytes: AtomicU64::new(0),
            current_month: AtomicU64::new(month),
            current_year: AtomicU64::new(year),
            monthly_limit,
            warning_threshold,
            storage_path: storage_path.to_string(),
        };
        
        // Load saved data if available
        tracker.load_data();
        
        // Check if month has changed and reset if needed
        tracker.check_and_reset_month();
        
        tracker
    }
    
    // Update the bandwidth counter with a new transfer
    pub fn add_bytes(&self, bytes: u64) -> bool {
        // First check if we've exceeded the warning threshold
        if self.monthly_bytes.load(Ordering::Relaxed) >= self.warning_threshold {
            return false; // Reject the transfer
        }
        
        // Add the bytes to the counter
        let new_total = self.monthly_bytes.fetch_add(bytes, Ordering::Relaxed) + bytes;
        
        // Save data periodically (e.g., every 1GB or so)
        if new_total % 1_000_000_000 < bytes {
            self.save_data();
        }
        
        // Check if we've now exceeded the warning threshold
        new_total < self.warning_threshold
    }
    
    // Check if the month has changed and reset counter if needed
    pub fn check_and_reset_month(&self) {
        let now = Utc::now();
        let current_month = now.month() as u64;
        let current_year = now.year() as u64;
        
        let stored_month = self.current_month.load(Ordering::Relaxed);
        let stored_year = self.current_year.load(Ordering::Relaxed);
        
        // If month or year has changed, reset the counter
        if current_month != stored_month || current_year != stored_year {
            // Store previous month's data for reporting
            let previous_total = self.monthly_bytes.load(Ordering::Relaxed);
            info!(
                "Month changed from {}/{} to {}/{}, resetting bandwidth counter. Previous usage: {} bytes",
                stored_month, stored_year, current_month, current_year, previous_total
            );
            
            // Reset counter and update month/year
            self.monthly_bytes.store(0, Ordering::Relaxed);
            self.current_month.store(current_month, Ordering::Relaxed);
            self.current_year.store(current_year, Ordering::Relaxed);
            
            // Save the updated data
            self.save_data();
        }
    }
    
    // Get current bandwidth usage info
    pub fn get_status(&self) -> (u64, u64, f64) {
        // Check if month needs to be reset
        self.check_and_reset_month();
        
        let bytes = self.monthly_bytes.load(Ordering::Relaxed);
        // Return current bytes, limit, and percentage used
        (
            bytes, 
            self.monthly_limit,
            (bytes as f64 / self.monthly_limit as f64) * 100.0
        )
    }
    
    // Save bandwidth data to disk
    fn save_data(&self) {
        let bytes = self.monthly_bytes.load(Ordering::Relaxed);
        let month = self.current_month.load(Ordering::Relaxed);
        let year = self.current_year.load(Ordering::Relaxed);
        
        let data = format!("{},{},{}", bytes, month, year);
        
        match File::create(&self.storage_path) {
            Ok(mut file) => {
                if let Err(e) = file.write_all(data.as_bytes()) {
                    error!("Failed to write bandwidth data: {}", e);
                }
            },
            Err(e) => {
                error!("Failed to create bandwidth data file: {}", e);
            }
        }
    }
    
    // Load bandwidth data from disk
    fn load_data(&mut self) {
        if !Path::new(&self.storage_path).exists() {
            return;
        }
        
        match File::open(&self.storage_path) {
            Ok(mut file) => {
                let mut data = String::new();
                if file.read_to_string(&mut data).is_ok() {
                    let parts: Vec<&str> = data.split(',').collect();
                    if parts.len() == 3 {
                        if let Ok(bytes) = parts[0].parse::<u64>() {
                            self.monthly_bytes = AtomicU64::new(bytes);
                        }
                        if let Ok(month) = parts[1].parse::<u64>() {
                            self.current_month = AtomicU64::new(month);
                        }
                        if let Ok(year) = parts[2].parse::<u64>() {
                            self.current_year = AtomicU64::new(year);
                        }
                    }
                }
            },
            Err(e) => {
                error!("Failed to open bandwidth data file: {}", e);
            }
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", content = "payload")]
pub enum WebSocketMessage {
    // Basic communication
    Register { hardware_id: String },
    RegisterResponse { device_id: String },
    Send(RelayMessage),
    Receive(RelayMessage),
    Ack { message_id: String },
    Ping,
    Pong,
    Error { message: String, code: Option<u16> },
    
    // Authentication with challenge-response
    AuthRequest {
        device_id: String,
        public_key: String,      // Base64 encoded public key
        challenge: String,       // Random challenge for verification
        hardware_id: String,     // Hardware ID hash
    },

    AuthResponse {
        device_id: String,
        challenge_response: String,  // Signed challenge
        challenge: String,           // New challenge for requester to sign
        hardware_id: String,         // Hardware ID hash
    },

    AuthVerify {
        device_id: String,
        challenge_response: String,  // Signed challenge
        hardware_id: String,         // Hardware ID hash
    },

    AuthSuccess {
        device_id: String,
        trusted: bool,               // Whether device is trusted
        hardware_id: String,         // Hardware ID hash of the trusted device
    },

    KeyRotationUpdate {
        sender_id: String,
        recipient_id: String,
        encrypted_key_package: String,  // Encrypted package containing new keys
        key_id: u64,                    // ID of the new key for tracking
        hardware_id: String,            // Hardware ID hash of the sender
    },

    KeyRotationAck {
        sender_id: String,
        recipient_id: String,
        key_id: u64,                    // ID of the acknowledged key
        success: bool,                  // Whether it was successful
        hardware_id: String,            // Hardware ID hash of the sender
    },

    Status {
        device_id: String,
        status: DeviceStatus,
        hardware_id: String,            // Hardware ID hash
    },
}

// Enhanced relay message with authentication, message type, and size tracking
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RelayMessage {
    pub recipient_id: String,
    pub sender_id: String,
    pub encrypted_data: String,
    pub message_id: Uuid,
    pub timestamp: u64,              // Unix timestamp of when message was created
    pub ttl: u64,                    // TTL
    
    // Authentication
    pub signature: Option<String>,   // Digital signature of the message
    pub hardware_id: String,         // Hardware ID hash of the sender
    
    // Limit enforcement
    pub content_type: ContentType,   // Type of content for rate limiting
    pub size_bytes: u64,             // Size of the encrypted_data in bytes
}

// Device identity information
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeviceIdentity {
    pub device_id: String,
    pub hardware_id: String,         // Hardware ID hash
    pub public_key: String,
    pub name: Option<String>,        // Optional device name
    pub verified: bool,              // Verified of not
    pub last_active: u64,            // Unix of last activity
    pub current_key_id: u64,         // Current encryption key ID
}

// Device status for presence information
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum DeviceStatus {
    Online,
    Away,
    Offline,
}

// Content type for rate limiting
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ContentType {
    Text,
    Image,
    KeyPackage,
    Other,
}

// Rate limiting with combined point-based quota and request limits
pub struct RateLimit {
    pub max_points_per_minute: u32,      // Total points available per minute (data volume)
    pub remaining_points: u32,           // Points remaining in current period
    pub max_requests_per_minute: u32,    // Maximum requests per minute (request frequency)
    pub request_counter: u32,            // Requests used in current period
    pub last_reset: Instant,             // When limits were last reset
}

impl Default for RateLimit {
    fn default() -> Self {
        Self {
            max_points_per_minute: 512,    // 512 points per minute (512KB)
            remaining_points: 512,         // Start with full points
            max_requests_per_minute: 60,   // 60 requests per minute (1 per second average)
            request_counter: 0,            // Start with 0 requests
            last_reset: Instant::now(),
        }
    }
}

impl RateLimit {
    // Check both request count and data size limits
    pub fn check_and_consume(&mut self, size_bytes: u64) -> Result<(), String> {
        // Reset counters if a minute has passed
        if self.last_reset.elapsed() >= Duration::from_secs(60) {
            self.remaining_points = self.max_points_per_minute;
            self.request_counter = 0;
            self.last_reset = Instant::now();
        }
        
        // First check request count limit
        if self.request_counter >= self.max_requests_per_minute {
            return Err(format!(
                "Request limit exceeded ({}/{}). Try again in a minute.",
                self.request_counter,
                self.max_requests_per_minute
            ));
        }
        
        // Then check data points limit
        let kb_size = (size_bytes as f64 / 1024.0).ceil() as u32;
        let points_needed = if kb_size == 0 { 1 } else { kb_size };
        
        if points_needed > self.remaining_points {
            return Err(format!(
                "Quota exceeded for message size {}KB. {}/{} points remaining. Try again in a minute.",
                kb_size,
                self.remaining_points,
                self.max_points_per_minute
            ));
        }
        
        // Both limits passed, update counters
        self.request_counter += 1;
        self.remaining_points -= points_needed;
        Ok(())
    }
    
    // Get current status for debugging/info
    pub fn get_status(&mut self) -> (u32, u32, u32, u32) {
        // Refresh if needed
        if self.last_reset.elapsed() >= Duration::from_secs(60) {
            self.remaining_points = self.max_points_per_minute;
            self.request_counter = 0;
            self.last_reset = Instant::now();
        }
        
        (
            self.remaining_points, 
            self.max_points_per_minute,
            self.request_counter,
            self.max_requests_per_minute
        )
    }
}

// Queue messages for offline devices
pub struct QueuedMessage {
    pub message: WebSocketMessage,
    pub timestamp: Instant,
    pub ttl: Duration,
}

impl QueuedMessage {
    pub fn new(message: WebSocketMessage, ttl_seconds: u64) -> Self {
        Self {
            message,
            timestamp: Instant::now(),
            ttl: Duration::from_secs(ttl_seconds),
        }
    }
    
    pub fn is_expired(&self) -> bool {
        self.timestamp.elapsed() >= self.ttl
    }
}
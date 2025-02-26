//! Provides the `FcmClient` struct for interacting with the FCM API.
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use yup_oauth2::authenticator::Authenticator;
use yup_oauth2::hyper::client::HttpConnector;
use yup_oauth2::hyper_rustls::HttpsConnector;
use yup_oauth2::{read_service_account_key, ServiceAccountAuthenticator};

/// Represents errors that can occur while using the FCM client.
#[derive(Error, Debug)]
pub enum FcmError {
    /// An error occurred during an HTTP request.
    #[error("Request failed: {0}")]
    RequestError(#[from] reqwest::Error),

    /// An error occurred during authentication.
    #[error("Authentication failed: {0}")]
    AuthError(String),

    /// An error occurred during JSON serialization or deserialization.
    #[error("JSON serialization/deserialization failed: {0}")]
    JsonError(#[from] serde_json::Error),

    /// An error occurred related to OAuth2.
    #[error("OAuth2 error: {0}")]
    OAuth2Error(#[from] yup_oauth2::Error),

    /// An error occurred during file I/O operations.
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    // An error in the response from FCM.
    #[error("Response error: {0}")]
    ResponseError(FcmErrorResponse),
}

/// Represents the result of a sent FCM message.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum FcmSendResult {
    /// A successful response from FCM.
    Success(FcmSuccessResponse),
    /// An error response from FCM.
    Error(FcmErrorResponse),
}

/// Represents a successful response from the FCM API after sending a message.
#[derive(Deserialize, Debug)]
pub struct FcmSuccessResponse {
    /// Message ID if the message was successfully processed
    pub name: String,
}

/// Represents an error response from the FCM API after sending a message.
#[derive(Serialize, Deserialize, Debug)]
pub struct FcmErrorResponse {
    /// Error if the message was unsuccessfully processed.
    pub error: ErrorResponse,
}

impl std::fmt::Display for FcmErrorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}: {:?}", self.error.message, self.error.details)
    }
}

/// Contains the details of an error response from FCM.
/// Visit the [FCM Documentation](https://firebase.google.com/docs/cloud-messaging/send-message#rest) for details on the possible errors the API can respond with.
#[derive(Serialize, Deserialize, Debug)]
pub struct ErrorResponse {
    /// The error code.
    pub code: usize,
    /// The error message.
    pub message: String,
    /// The error status
    pub status: String,
    /// Additional details about the error.
    pub details: Vec<Value>,
}
/// Firebase Cloud Messaging (FCM) client.
pub struct FcmClient {
    /// Authenticator for OAuth2.
    auth: Authenticator<HttpsConnector<HttpConnector>>,
    /// HTTP client for making requests.
    http_client: Client,
    /// Firebase project ID.
    project_id: String,
}

impl FcmClient {
    /// Creates a new `FcmClient` instance.
    ///
    /// # Arguments
    ///
    /// * `service_account_key_path` - Path to the service account key JSON file.
    ///
    /// # Errors
    ///
    /// Returns an `FcmError` if:
    ///
    /// * The service account key cannot be read.
    /// * The authenticator cannot be built.
    /// * Any other error occurs during initialization.
    pub async fn new(service_account_key_path: &str) -> Result<Self, FcmError> {
        let secret = read_service_account_key(service_account_key_path).await?;
        let project_id = match secret.project_id {
            Some(ref id) => id.clone(),
            None => {
                return Err(FcmError::AuthError(
                    "Service account key JSON file missing project ID".to_string(),
                ));
            }
        };
        let auth = ServiceAccountAuthenticator::builder(secret).build().await?;
        Ok(Self {
            auth,
            http_client: Client::new(),
            project_id,
        })
    }

    /// Sends an FCM message.
    ///
    /// This method constructs and sends an HTTP request to the FCM API to deliver a
    /// message to the specified recipients. It handles authentication, constructs the
    /// necessary request, and processes the response from the FCM service.
    ///
    /// # Arguments
    ///
    /// * `message` - The `Message` to send.
    ///
    /// # Errors
    ///
    /// Returns an `FcmError` if there's an issue with the request, authentication,
    /// JSON (de)serialization, the response, or any other error during the sending process.
    pub async fn send(&self, body: Value) -> Result<FcmSuccessResponse, FcmError> {
        let url = self.build_url();
        let token_str = self.get_token().await?;

        let response = self
            .http_client
            .post(url)
            .header("Authorization", format!("Bearer {:?}", token_str))
            .json(&body) // Send the request object
            .send()
            .await?;

        let response = response.json::<FcmSendResult>().await;
        match response {
            Ok(FcmSendResult::Success(success)) => Ok(success),
            Ok(FcmSendResult::Error(error)) => Err(FcmError::ResponseError(error)),
            Err(e) => Err(FcmError::RequestError(e)),
        }
    }

    /// Constructs the URL for sending FCM messages.
    ///
    /// # Returns
    ///
    /// Returns a `String` containing the URL for the FCM API endpoint.
    fn build_url(&self) -> String {
        format!(
            "https://fcm.googleapis.com/v1/projects/{}/messages:send",
            self.project_id
        )
    }

    /// Retrieves an OAuth2 token for the FCM API.
    ///
    /// This method requests an OAuth2 token from the authenticator that is required to authenticate
    /// requests to the FCM API.
    ///
    /// # Errors
    ///
    /// Returns an `FcmError` if:
    ///
    /// * The token cannot be retrieved from the authenticator.
    /// * Any other error occurs while fetching the token.
    ///
    /// # Returns
    ///
    /// On success, returns the OAuth2 token as a `String`. If the token cannot be retrieved,
    /// an `FcmError` is returned.
    async fn get_token(&self) -> Result<String, FcmError> {
        let token = self
            .auth
            .token(&["https://www.googleapis.com/auth/firebase.messaging"])
            .await?;

        token.token().map(|s| s.to_string()).ok_or_else(|| {
            FcmError::AuthError("Failed to retrieve token from authenticator".to_string())
        })
    }
}

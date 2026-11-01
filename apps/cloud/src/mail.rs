//! Optional SMTP for verification and password-reset emails. Configure with
//! OKYTES_CLOUD_SMTP_URL (for example `smtps://user:password@smtp.example.com`)
//! and OKYTES_CLOUD_MAIL_FROM. Without it, accounts need no verification and
//! the owner issues reset links with `okytes-cloud reset-link`.
use anyhow::{Context, Result};
use lettre::{
    message::header::ContentType, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};

pub struct Mailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: lettre::message::Mailbox,
}

impl Mailer {
    pub fn from_environment() -> Result<Option<Self>> {
        let (Ok(url), Ok(from)) = (
            std::env::var("OKYTES_CLOUD_SMTP_URL"),
            std::env::var("OKYTES_CLOUD_MAIL_FROM"),
        ) else {
            return Ok(None);
        };
        Ok(Some(Self {
            transport: AsyncSmtpTransport::<Tokio1Executor>::from_url(&url)
                .context("OKYTES_CLOUD_SMTP_URL is not a valid smtp:// or smtps:// URL")?
                .timeout(Some(std::time::Duration::from_secs(20)))
                .build(),
            from: from
                .parse()
                .context("OKYTES_CLOUD_MAIL_FROM is not an email address")?,
        }))
    }

    pub async fn send(&self, to: &str, subject: &str, body: String) -> Result<()> {
        let message = Message::builder()
            .from(self.from.clone())
            .to(to.parse().context("recipient address")?)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body)?;
        self.transport.send(message).await?;
        Ok(())
    }
}

# Security Policy

## Supported Versions

We actively maintain and provide security updates for the following versions of PRISM:

| Version | Supported          |
| ------- | ------------------ |
| 2.0.x   | :white_check_mark: |
| 1.x     | :x:                |
| < 1.0   | :x:                |

## Reporting a Vulnerability

We take the security of PRISM seriously. If you believe you have found a security vulnerability, please report it to us as described below.

### How to Report

**Please do not report security vulnerabilities through public GitHub issues.**

Instead, please report them via email to **team@marc27.com** with the subject line "PRISM Security Vulnerability Report".

### What to Include

Please include the following information in your report:

- **Description**: A clear description of the vulnerability
- **Impact**: What an attacker could accomplish by exploiting this vulnerability
- **Steps to Reproduce**: Step-by-step instructions to reproduce the issue
- **Affected Components**: Which parts of PRISM are affected
- **Environment**: Version information and configuration details
- **Proof of Concept**: If possible, include a minimal example demonstrating the vulnerability

### Our Response Process

1. **Acknowledgment**: We will acknowledge receipt of your vulnerability report within 48 hours
2. **Investigation**: We will investigate the reported vulnerability and determine its severity
3. **Timeline**: We aim to provide regular updates on our progress every 5-7 days
4. **Resolution**: We will work to resolve confirmed vulnerabilities as quickly as possible
5. **Disclosure**: Once a fix is available, we will coordinate responsible disclosure

### Security Considerations

#### API Keys and Authentication

- **LLM Provider API Keys**: PRISM requires API keys for various LLM providers (OpenAI, Anthropic, Google Vertex AI, OpenRouter). These keys are stored locally and should be kept secure
- **Environment Variables**: API keys are configured through environment variables and should never be committed to version control
- **Key Rotation**: Regularly rotate your API keys and update them in your PRISM configuration

#### Data Handling

- **Materials Data**: PRISM queries public materials science databases through the OPTIMADE API
- **Local Storage**: Results can be stored in a local SQLite database for analysis
- **No Sensitive Data**: PRISM does not collect or transmit personal information beyond what's necessary for API functionality

#### Network Security

- **HTTPS Communications**: All API communications use HTTPS encryption
- **Rate Limiting**: Built-in rate limiting prevents abuse of external APIs
- **Input Validation**: User inputs are validated before being processed or sent to external services

#### Dependencies

- **Regular Updates**: We regularly update dependencies to address known security vulnerabilities
- **Vulnerability Scanning**: Dependencies are monitored for security issues
- **Minimal Dependencies**: We maintain a minimal dependency footprint to reduce attack surface

### Security Best Practices for Users

#### Installation and Setup

- Install PRISM from trusted sources only (official repository or PyPI)
- Use virtual environments to isolate PRISM dependencies
- Keep your installation updated to the latest version

#### API Key Management

- Store API keys securely using environment variables or secure credential management systems
- Never commit API keys to version control
- Use separate API keys for development and production environments
- Monitor API key usage for unusual activity

#### Database Security

- If using the local database feature, ensure proper file permissions on the SQLite database
- Consider encryption at rest for sensitive research data
- Regularly backup your local database

#### Network Configuration

- Be aware of your network environment when using PRISM
- Consider using VPN or secure networks when working with proprietary research data
- Monitor network traffic if working in sensitive environments

### Scope

This security policy covers:

- The PRISM core application (`app/` directory)
- CLI interface and commands
- API integrations with LLM providers
- OPTIMADE database connectors
- MCP server and client (`prism serve`, external MCP connections)
- CALPHAD thermodynamic calculations (pycalphad, TDB file handling)
- ML pipeline (model training, predictions, feature engineering)
- Plugin system (entry-point and local `~/.prism/plugins/` plugins)
- Local data storage and processing

### Out of Scope

The following are outside the scope of our security policy:

- Security of third-party LLM providers or OPTIMADE databases
- Security of the user's local environment or network
- Issues in dependencies that do not affect PRISM's security
- General Python or operating system security issues

### Security Updates

Security updates will be released as patch versions and announced through:

- GitHub Security Advisories
- Release notes and changelog
- Email notifications to maintainers
- Project documentation updates

### Contact Information

For security-related questions or concerns:

- **Security Reports**: team@marc27.com
- **General Questions**: [GitHub Issues](https://github.com/Darth-Hidious/PRISM/issues) (for non-security matters only)
- **Project Homepage**: https://github.com/Darth-Hidious/PRISM

---

*This security policy is effective as of the date of publication and may be updated periodically to reflect changes in our security practices or the project structure.*
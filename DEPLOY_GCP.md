# Deploying Prism Server to Google Cloud Run

This guide walks you through deploying the Prism document processing server to Google Cloud Run with auto-scaling.

## Prerequisites

- [Google Cloud SDK](https://cloud.google.com/sdk/docs/install) (`gcloud` CLI)
- Docker Desktop (for local builds) or Cloud Build (for cloud builds)
- A Google Cloud project with billing enabled

## Quick Deploy (Recommended)

### 1. Authenticate and Set Project

```bash
# Login to Google Cloud
gcloud auth login

# Set your project
gcloud config set project YOUR_PROJECT_ID

# Enable required APIs
gcloud services enable cloudbuild.googleapis.com run.googleapis.com containerregistry.googleapis.com
```

### 2. Deploy via Cloud Build

```bash
# From the Prism root directory
cd c:/Dev/RustSandbox/Prism

# Submit build (builds in cloud, no Docker needed locally)
gcloud builds submit --config cloudbuild.yaml
```

This will:
- Build the Docker image in Google Cloud Build
- Push to Google Container Registry
- Deploy to Cloud Run with auto-scaling (0-10 instances)

### 3. Get Your Service URL

```bash
gcloud run services describe prism-server --region us-central1 --format="value(status.url)"
```

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `PRISM_HOST` | `0.0.0.0` | Bind address (keep default for Cloud Run) |
| `PRISM_PORT` | `8080` | Port (keep default for Cloud Run) |
| `PRISM_CORS_ORIGINS` | *(empty)* | Allowed origins. Empty = documain.ai only. Use `*` for dev. |

### CORS Configuration

**Default (Production)**: Only allows requests from `documain.ai` and all subdomains.

**Development Mode**: Set `PRISM_CORS_ORIGINS=*` to allow all origins:

```bash
gcloud run services update prism-server \
  --region us-central1 \
  --set-env-vars="PRISM_CORS_ORIGINS=*"
```

**Custom Origins**: Comma-separated list:

```bash
gcloud run services update prism-server \
  --region us-central1 \
  --set-env-vars="PRISM_CORS_ORIGINS=https://app.example.com,https://www.example.com"
```

### Scaling Configuration

Adjust scaling via Cloud Console or CLI:

```bash
gcloud run services update prism-server \
  --region us-central1 \
  --min-instances=1 \      # Keep 1 warm instance (faster cold starts)
  --max-instances=20 \     # Scale up to 20 under load
  --memory=4Gi \           # More memory for large documents
  --cpu=4                   # More CPU for complex parsing
```

## Manual Deployment (Alternative)

If you prefer to build locally with Docker:

```bash
# Build image
docker build -t gcr.io/YOUR_PROJECT_ID/prism-server:latest .

# Push to Container Registry
docker push gcr.io/YOUR_PROJECT_ID/prism-server:latest

# Deploy to Cloud Run
gcloud run deploy prism-server \
  --image gcr.io/YOUR_PROJECT_ID/prism-server:latest \
  --region us-central1 \
  --platform managed \
  --allow-unauthenticated \
  --memory 2Gi \
  --cpu 2 \
  --min-instances 0 \
  --max-instances 10
```

## Testing the Deployment

### Health Check

```bash
SERVICE_URL=$(gcloud run services describe prism-server --region us-central1 --format="value(status.url)")

curl $SERVICE_URL/api/health
# Expected: {"status":"ok","version":"0.1.0"}
```

### CORS Test

```bash
# Should succeed (correct origin)
curl -H "Origin: https://documain.ai" -I $SERVICE_URL/api/health

# Should fail CORS (wrong origin)
curl -H "Origin: https://evil.com" -I $SERVICE_URL/api/health
```

### Document Conversion

```bash
curl -X POST $SERVICE_URL/api/convert \
  -F "file=@test-files/sample.docx" \
  --output result.html
```

## Monitoring

### View Logs

```bash
gcloud run services logs read prism-server --region us-central1 --limit=50
```

### View in Cloud Console

Visit: https://console.cloud.google.com/run/detail/us-central1/prism-server/logs

## Cost Optimization

- **Scale to zero**: Default config uses `min-instances=0`, so you pay nothing when idle
- **Right-size memory**: Start with 2GB, increase only if needed
- **Use Cloud Build**: Faster than local Docker builds, billed by minute

## Troubleshooting

### Container fails to start

Check logs:
```bash
gcloud run services logs read prism-server --region us-central1
```

### CORS errors in browser

1. Check `PRISM_CORS_ORIGINS` env var is set correctly
2. Ensure your domain uses HTTPS
3. Check browser Network tab for actual origin being sent

### Build timeout

Rust builds can take 15-20 minutes. The `cloudbuild.yaml` sets a 30-minute timeout and uses a high-CPU machine.

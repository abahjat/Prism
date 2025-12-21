// Prism Document Viewer JavaScript

const API_BASE_URL = 'http://localhost:8080';

// DOM Elements
const fileInput = document.getElementById('fileInput');
const selectFileBtn = document.getElementById('selectFileBtn');
const uploadBox = document.getElementById('uploadBox');
const statusSection = document.getElementById('statusSection');
const statusText = document.getElementById('statusText');
const fileInfo = document.getElementById('fileInfo');
const fileName = document.getElementById('fileName');
const fileSize = document.getElementById('fileSize');
const fileFormat = document.getElementById('fileFormat');
const viewerSection = document.getElementById('viewerSection');
const viewerContent = document.getElementById('viewerContent');
const errorSection = document.getElementById('errorSection');
const errorMessage = document.getElementById('errorMessage');
const clearBtn = document.getElementById('clearBtn');
const retryBtn = document.getElementById('retryBtn');
const serverStatus = document.getElementById('serverStatus');

// Initialize
checkServerStatus();

// Event Listeners
selectFileBtn.addEventListener('click', () => {
    fileInput.click();
});

fileInput.addEventListener('change', (e) => {
    if (e.target.files.length > 0) {
        handleFile(e.target.files[0]);
    }
});

clearBtn.addEventListener('click', resetViewer);
retryBtn.addEventListener('click', resetViewer);

// Drag and Drop
uploadBox.addEventListener('dragover', (e) => {
    e.preventDefault();
    uploadBox.classList.add('dragover');
});

uploadBox.addEventListener('dragleave', () => {
    uploadBox.classList.remove('dragover');
});

uploadBox.addEventListener('drop', (e) => {
    e.preventDefault();
    uploadBox.classList.remove('dragover');

    if (e.dataTransfer.files.length > 0) {
        handleFile(e.dataTransfer.files[0]);
    }
});

// Check if server is running
async function checkServerStatus() {
    try {
        const response = await fetch(`${API_BASE_URL}/health`);
        if (response.ok) {
            serverStatus.textContent = 'Online ✓';
            serverStatus.classList.add('online');
            serverStatus.classList.remove('offline');
        } else {
            throw new Error('Server not responding');
        }
    } catch (error) {
        serverStatus.textContent = 'Offline ✗';
        serverStatus.classList.add('offline');
        serverStatus.classList.remove('online');
        console.error('Server check failed:', error);
    }
}

// Handle file upload
async function handleFile(file) {
    console.log('Processing file:', file.name);

    // Hide all sections
    hideAllSections();

    // Show status
    statusSection.style.display = 'block';
    statusText.textContent = `Processing ${file.name}...`;

    try {
        // Upload file to Prism server
        const formData = new FormData();
        formData.append('file', file);

        const response = await fetch(`${API_BASE_URL}/convert`, {
            method: 'POST',
            body: formData,
        });

        if (!response.ok) {
            // Try to get error message from response
            let errorMsg = `Server error: ${response.status}`;
            try {
                const errorData = await response.json();
                errorMsg = errorData.message || errorMsg;
            } catch (e) {
                // If JSON parsing fails, use default error
            }
            throw new Error(errorMsg);
        }

        // Get content type to determine how to display
        const contentType = response.headers.get('content-type');

        if (contentType && contentType.includes('text/html')) {
            // HTML response - display in iframe
            const html = await response.text();
            displayDocument(file, html);
        } else if (contentType && contentType.includes('application/json')) {
            // JSON response (fallback mode - format detected but no parser)
            const data = await response.json();
            displayFormatInfo(file, data);
        } else {
            throw new Error('Unexpected response type from server');
        }

    } catch (error) {
        console.error('Error processing file:', error);
        showError(error.message);
    }
}

// Display document content
function displayDocument(file, html) {
    hideAllSections();

    // Show file info
    fileInfo.style.display = 'flex';
    fileName.textContent = file.name;
    fileSize.textContent = formatFileSize(file.size);
    fileFormat.textContent = getFileExtension(file.name).toUpperCase();

    // Show viewer with iframe
    viewerSection.style.display = 'block';

    // Create iframe to display HTML
    const iframe = document.createElement('iframe');
    iframe.style.width = '100%';
    iframe.style.border = 'none';
    iframe.style.minHeight = '600px';

    // Write HTML to iframe
    viewerContent.innerHTML = '';
    viewerContent.appendChild(iframe);

    const iframeDoc = iframe.contentDocument || iframe.contentWindow.document;
    iframeDoc.open();
    iframeDoc.write(html);
    iframeDoc.close();

    // Adjust iframe height to content
    iframe.onload = () => {
        try {
            const height = iframe.contentWindow.document.documentElement.scrollHeight;
            iframe.style.height = Math.max(height, 600) + 'px';
        } catch (e) {
            console.warn('Could not adjust iframe height:', e);
        }
    };
}

// Display format detection info (fallback mode)
function displayFormatInfo(file, data) {
    hideAllSections();

    // Show file info
    fileInfo.style.display = 'flex';
    fileName.textContent = file.name;
    fileSize.textContent = formatFileSize(file.size);
    fileFormat.textContent = data.format.name || 'Unknown';

    // Show viewer with format info
    viewerSection.style.display = 'block';

    viewerContent.innerHTML = `
        <div style="padding: 2rem; text-align: center;">
            <h3 style="color: #667eea; margin-bottom: 1rem;">Format Detected</h3>
            <div style="background: #f8f9fa; padding: 1.5rem; border-radius: 8px; text-align: left; max-width: 600px; margin: 0 auto;">
                <p style="margin-bottom: 0.5rem;"><strong>Format:</strong> ${data.format.name}</p>
                <p style="margin-bottom: 0.5rem;"><strong>MIME Type:</strong> ${data.format.mime_type}</p>
                <p style="margin-bottom: 0.5rem;"><strong>Extension:</strong> .${data.format.extension}</p>
                <p style="margin-bottom: 0.5rem;"><strong>Family:</strong> ${data.format.family}</p>
                <p style="margin-bottom: 0.5rem;"><strong>Confidence:</strong> ${(data.confidence * 100).toFixed(1)}%</p>
                <p style="margin-bottom: 0.5rem;"><strong>Detection Method:</strong> ${data.method}</p>
            </div>
            <p style="margin-top: 1.5rem; color: #666; font-style: italic;">
                ${data.message}
            </p>
        </div>
    `;
}

// Show error
function showError(message) {
    hideAllSections();
    errorSection.style.display = 'block';
    errorMessage.textContent = message;
}

// Reset viewer
function resetViewer() {
    hideAllSections();
    uploadBox.style.display = 'block';
    fileInput.value = '';
}

// Hide all sections
function hideAllSections() {
    uploadBox.style.display = 'none';
    statusSection.style.display = 'none';
    fileInfo.style.display = 'none';
    viewerSection.style.display = 'none';
    errorSection.style.display = 'none';
}

// Utility functions
function formatFileSize(bytes) {
    if (bytes === 0) return '0 Bytes';
    const k = 1024;
    const sizes = ['Bytes', 'KB', 'MB', 'GB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return Math.round(bytes / Math.pow(k, i) * 100) / 100 + ' ' + sizes[i];
}

function getFileExtension(filename) {
    const parts = filename.split('.');
    return parts.length > 1 ? parts[parts.length - 1] : '';
}

// Periodic server status check (every 30 seconds)
setInterval(checkServerStatus, 30000);

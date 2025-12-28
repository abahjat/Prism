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

// Zoom control elements
const zoomControls = document.getElementById('zoomControls');
const zoomInBtn = document.getElementById('zoomInBtn');
const zoomOutBtn = document.getElementById('zoomOutBtn');
const zoomLevel = document.getElementById('zoomLevel');
const fitBtn = document.getElementById('fitBtn');
const actualSizeBtn = document.getElementById('actualSizeBtn');

// Zoom state
let currentZoom = 1;
let isImageMode = false;
let imageNaturalWidth = 0;
let imageNaturalHeight = 0;

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

// Zoom control event listeners
zoomInBtn.addEventListener('click', () => zoomImage(1.25));
zoomOutBtn.addEventListener('click', () => zoomImage(0.8));
fitBtn.addEventListener('click', fitToWindow);
actualSizeBtn.addEventListener('click', actualSize);

// Mouse wheel zoom for images
viewerContent.addEventListener('wheel', (e) => {
    if (isImageMode) {
        e.preventDefault();
        const factor = e.deltaY < 0 ? 1.1 : 0.9;
        zoomImage(factor);
    }
});

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
        const response = await fetch(`${API_BASE_URL}/api/health`);
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

        const response = await fetch(`${API_BASE_URL}/api/convert`, {
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

    // Check if this is an image file
    const ext = getFileExtension(file.name).toLowerCase();
    const imageExtensions = ['png', 'jpg', 'jpeg', 'tif', 'tiff', 'gif', 'bmp', 'webp'];
    isImageMode = imageExtensions.includes(ext);

    // Show file info
    fileInfo.style.display = 'flex';
    fileName.textContent = file.name;
    fileSize.textContent = formatFileSize(file.size);
    fileFormat.textContent = ext.toUpperCase();

    // Show viewer
    viewerSection.style.display = 'block';

    if (isImageMode) {
        // Image mode: display with zoom controls
        zoomControls.style.display = 'flex';
        viewerContent.classList.add('image-mode');
        viewerContent.innerHTML = '';

        // Parse the HTML to extract image src
        const parser = new DOMParser();
        const doc = parser.parseFromString(html, 'text/html');
        const imgEl = doc.querySelector('img');

        if (imgEl && imgEl.src) {
            // Create zoom container and image
            const container = document.createElement('div');
            container.className = 'image-zoom-container';

            const img = document.createElement('img');
            img.src = imgEl.src;
            img.alt = imgEl.alt || file.name;

            img.onload = () => {
                imageNaturalWidth = img.naturalWidth;
                imageNaturalHeight = img.naturalHeight;
                // Fit to window on initial load
                fitToWindow();
            };

            container.appendChild(img);
            viewerContent.appendChild(container);
        } else {
            // Fallback to iframe if no image found
            displayAsIframe(html);
        }
    } else {
        // Regular document mode: use iframe
        zoomControls.style.display = 'none';
        viewerContent.classList.remove('image-mode');
        displayAsIframe(html);
    }
}

// Display content in iframe (for non-image documents)
function displayAsIframe(html) {
    viewerContent.innerHTML = '';

    const iframe = document.createElement('iframe');
    iframe.style.width = '100%';
    iframe.style.border = 'none';
    iframe.style.minHeight = '600px';

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

    // Build preview section if available
    let previewHtml = '';
    if (data.preview) {
        previewHtml = `
            <div style="margin-top: 1.5rem;">
                <h4 style="color: #667eea; margin-bottom: 0.5rem;">File Content Preview</h4>
                <pre style="background: #1a1a2e; color: #00ff88; padding: 1rem; border-radius: 8px; overflow-x: auto; text-align: left; font-family: 'Courier New', monospace; font-size: 12px; max-height: 400px; overflow-y: auto; white-space: pre-wrap; word-wrap: break-word;">${escapeHtml(data.preview)}</pre>
            </div>
        `;
    }

    viewerContent.innerHTML = `
        <div style="padding: 2rem; text-align: center;">
            <h3 style="color: #667eea; margin-bottom: 1rem;">Format Detected</h3>
            <div style="background: #f8f9fa; padding: 1.5rem; border-radius: 8px; text-align: left; max-width: 600px; margin: 0 auto;">
                <p style="margin-bottom: 0.5rem;"><strong>Format:</strong> ${escapeHtml(data.format.name)}</p>
                <p style="margin-bottom: 0.5rem;"><strong>MIME Type:</strong> ${escapeHtml(data.format.mime_type)}</p>
                <p style="margin-bottom: 0.5rem;"><strong>Extension:</strong> .${escapeHtml(data.format.extension)}</p>
                <p style="margin-bottom: 0.5rem;"><strong>Family:</strong> ${escapeHtml(data.format.family)}</p>
                <p style="margin-bottom: 0.5rem;"><strong>Confidence:</strong> ${(data.confidence * 100).toFixed(1)}%</p>
                <p style="margin-bottom: 0.5rem;"><strong>Detection Method:</strong> ${escapeHtml(data.method)}</p>
            </div>
            <p style="margin-top: 1.5rem; color: #666; font-style: italic;">
                ${escapeHtml(data.message)}
            </p>
            ${previewHtml}
        </div>
    `;
}

// Escape HTML to prevent XSS
function escapeHtml(text) {
    if (!text) return '';
    const div = document.createElement('div');
    div.textContent = text;
    return div.innerHTML;
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
    resetImageState();
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

// =====================
// Image Zoom Functions
// =====================

// Zoom image by a factor
function zoomImage(factor) {
    if (!isImageMode) return;

    currentZoom = Math.max(0.1, Math.min(10, currentZoom * factor));
    applyZoom();
}

// Fit image to container window
function fitToWindow() {
    if (!isImageMode || imageNaturalWidth === 0 || imageNaturalHeight === 0) return;

    const containerWidth = viewerContent.clientWidth - 40; // Padding
    const containerHeight = viewerContent.clientHeight - 40;

    const scaleX = containerWidth / imageNaturalWidth;
    const scaleY = containerHeight / imageNaturalHeight;

    currentZoom = Math.min(scaleX, scaleY, 1); // Don't scale up, only down
    applyZoom();
}

// Show image at actual size (100%)
function actualSize() {
    if (!isImageMode) return;

    currentZoom = 1;
    applyZoom();
}

// Apply current zoom level to image
function applyZoom() {
    const container = viewerContent.querySelector('.image-zoom-container');
    if (!container) return;

    container.style.transform = `scale(${currentZoom})`;
    zoomLevel.textContent = Math.round(currentZoom * 100) + '%';
}

// Reset image zoom state
function resetImageState() {
    currentZoom = 1;
    isImageMode = false;
    imageNaturalWidth = 0;
    imageNaturalHeight = 0;
    viewerContent.classList.remove('image-mode');
    zoomControls.style.display = 'none';
}

// Periodic server status check (every 30 seconds)
setInterval(checkServerStatus, 30000);

const videoTypes = {
  mp4: 'video/mp4', m4v: 'video/x-m4v', mov: 'video/quicktime',
  webm: 'video/webm', ogv: 'video/ogg', avi: 'video/x-msvideo',
  mkv: 'video/x-matroska', mpeg: 'video/mpeg', mpg: 'video/mpeg',
  '3gp': 'video/3gpp', '3g2': 'video/3gpp2',
}

export function attachmentContentType(attachment) {
  const type = (attachment?.type || '').toLowerCase()
  if (type && type !== 'application/octet-stream') return type
  return videoTypes[attachment?.name?.split('.').pop()?.toLowerCase()] || type || 'application/octet-stream'
}

export const isVideoAttachment = (attachment) => attachmentContentType(attachment).startsWith('video/')

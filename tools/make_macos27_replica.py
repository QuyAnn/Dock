import cv2, numpy as np
ref = cv2.imread('/workspace/cc27_crop.jpg').astype(np.float32)
H,W = ref.shape[:2]; s = W/936.0
tiles = [(540,68,800,104,18),(428,118,660,222,52),(428,244,660,348,52),(428,370,660,474,52),(681,118,913,348,40),
 (680,370,787,474,52),(807,370,913,474,52),(428,496,535,600,52),(554,496,660,600,52),(681,496,913,600,52),
 (428,622,913,726,40),(428,748,913,852,40),(593,881,748,918,18),(490,0,936,46,0)]
mask = np.zeros((H,W),np.uint8)
for x0,y0,x1,y1,r in tiles:
    x0,y0,x1,y1,r=[int(round(v*s)) for v in (x0,y0,x1,y1,r)]
    cv2.rectangle(mask,(x0+r,y0),(x1-r,y1),255,-1); cv2.rectangle(mask,(x0,y0+r),(x1,y1-r),255,-1)
    for cx,cy in [(x0+r,y0+r),(x1-r,y0+r),(x0+r,y1-r),(x1-r,y1-r)]:
        if r>0: cv2.circle(mask,(cx,cy),r,255,-1)
mask = cv2.dilate(mask,np.ones((9,9),np.uint8))
known = (mask==0).astype(np.float32)
# push-pull fill: low-frequency reconstruction of the wallpaper under the tiles from the visible gaps
def pushpull(img,w):
    if min(img.shape[:2])<4: 
        m=(img*w[...,None]).sum((0,1))/max(w.sum(),1e-3); return np.broadcast_to(m,img.shape).copy()
    sm_w = cv2.resize(w,(img.shape[1]//2,img.shape[0]//2),interpolation=cv2.INTER_AREA)
    sm = cv2.resize(img*w[...,None],(img.shape[1]//2,img.shape[0]//2),interpolation=cv2.INTER_AREA)
    sm = np.where(sm_w[...,None]>1e-4, sm/np.maximum(sm_w[...,None],1e-4), 0)
    coarse = pushpull(sm, np.minimum(sm_w*4,1))
    up = cv2.resize(coarse,(img.shape[1],img.shape[0]),interpolation=cv2.INTER_LINEAR)
    return img*w[...,None] + up*(1-w[...,None])
fill = pushpull(ref, known)
fill = cv2.GaussianBlur(fill,(0,0),3)
out = np.where(known[...,None]>0, ref, fill)
out = cv2.GaussianBlur(out,(0,0),2.5)
sc = (1525-979)/(1099-515)
big = cv2.resize(out,None,fx=sc,fy=sc,interpolation=cv2.INTER_AREA)
ox = int(round(979-515*sc)); oy = int(round(113-142*sc))
canvas = cv2.warpAffine(big,np.float32([[1,0,ox],[0,1,oy]]),(1620,1080),borderMode=cv2.BORDER_REFLECT)
cv2.imwrite('macos27_replica.jpg',np.clip(canvas,0,255).astype(np.uint8),[cv2.IMWRITE_JPEG_QUALITY,92])
cv2.imwrite('/tmp/rep_v3.jpg',np.clip(canvas,0,255).astype(np.uint8))

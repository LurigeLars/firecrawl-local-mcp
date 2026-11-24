"""Synthetic OCR smoke test; no live source or production dependency changes."""
import io, json, os, subprocess, tempfile, time
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont, ImageFilter
from reportlab.pdfgen import canvas
from reportlab.lib.utils import ImageReader

ROWS = [('Crude oil','421.5','-1.2'), ('Gasoline','210.0','+0.8'), ('Distillate','118.7','-0.3'), ('Net position','-12,345','0')]
MARKER = 'Report date: 2026-09-08'
FONT='/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf'

def normalized(s): return ' '.join(s.split())
def score(s):
    lines=[normalized(l) for l in s.splitlines() if l.strip()]
    return {'date_ok': MARKER in lines, 'rows_exact': sum(' '.join(row) in lines for row in ROWS), 'rows_expected': len(ROWS), 'text': s.strip()}

def run(args, timeout=15):
    env={**os.environ,'OMP_THREAD_LIMIT':'1'}
    start=time.perf_counter()
    r=subprocess.run(args,check=True,capture_output=True,timeout=timeout,env=env)
    return r.stdout.decode('utf-8','replace'), round((time.perf_counter()-start)*1000)

def fixture(path):
    c=canvas.Canvas(str(path),pagesize=(612,792))
    c.setFont('Helvetica',14); c.drawString(36,740,'Native text control: report date 2026-09-08'); c.showPage()
    for degraded in (False,True):
        im=Image.new('RGB',(1530,1980),'white');d=ImageDraw.Draw(im)
        f=ImageFont.truetype(FONT,32); h=ImageFont.truetype(FONT,40)
        d.text((90,110),'Synthetic weekly petroleum report',font=h,fill='black')
        d.text((90,200),MARKER,font=f,fill='black')
        d.text((90,300),'Series',font=f,fill='black');d.text((720,300),'Level',font=f,fill='black');d.text((1080,300),'Change',font=f,fill='black')
        for i,(label,level,change) in enumerate(ROWS):
            y=380+85*i
            for x,t in zip((90,720,1080),(label,level,change)): d.text((x,y),t,font=f,fill='black')
        if degraded:
            im=im.resize((765,990),Image.Resampling.LANCZOS).filter(ImageFilter.GaussianBlur(.7))
            buf=io.BytesIO();im.save(buf,format='JPEG',quality=35);buf.seek(0);im=Image.open(buf)
        c.drawImage(ImageReader(im),0,0,width=612,height=792);c.showPage()
    c.save()

if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix='ocr-spike-') as td:
        p=Path(td); pdf=p/'mixed.pdf';fixture(pdf)
        native,_=run(['pdftotext','-f','1','-l','1',str(pdf),'-'])
        assert 'Native text control' in native
        results=[]
        for page in (2,3):
            text,_=run(['pdftotext','-f',str(page),'-l',str(page),str(pdf),'-'])
            assert not text.strip(), 'fixture unexpectedly has text layer'
            for dpi in (200,300):
                prefix=p/f'p{page}-{dpi}'
                _,render_ms=run(['pdftoppm','-f',str(page),'-l',str(page),'-r',str(dpi),'-singlefile','-gray','-png',str(pdf),str(prefix)])
                img=prefix.with_suffix('.png')
                with Image.open(img) as im:
                    pixels=im.width*im.height
                    assert pixels <= 16_000_000
                for psm in (3,6):
                    s,ocr_ms=run(['tesseract',str(img),'stdout','-l','eng','--psm',str(psm)])
                    results.append({'page':page,'degraded':page==3,'dpi':dpi,'psm':psm,'pixels':pixels,'render_ms':render_ms,'ocr_ms':ocr_ms,**score(s)})
        print(json.dumps({'native_text_ok':True,'scanned_text_layers_empty':True,'results':results},indent=2))

//! Hand-drawn widget recipes. Coordinates are DIPs at the common 42-DIP tile height.
//! Dock and gallery call this same painter; reference files are never read at runtime.
use chrono::{Datelike, Local, Timelike};
use windows::core::Result;
use super::dock::{RectF, TileView};
use super::renderer::{Align, Renderer, TextStyle};
use super::svgpath;

const TEXT:u32=0xF5F5_F7FF;
const MUTED:u32=0xAAAA_AEFF;
const TRACK:u32=0xFFFF_FF18;
const BLUE:u32=0x008A_FFFF;
const CYAN:u32=0x46C4_E6FF;
const RED:u32=0xFF45_3AFF;
const GREEN:u32=0x29CD_50FF;
const ORANGE:u32=0xFFA5_00FF;

/// Primary reference for every supported widget (also used by the visual audit).
pub fn reference(id:&str)->Option<&'static str> { Some(match id {
    "clock"=>"clock", "clockwide"=>"today", "clockweather"=>"clock-and-weather", "date"=>"date", "git"=>"github-activity",
    "todo"=>"todos", "focus"|"pomodoro"=>"pomodoro", "timer"=>"timer",
    "downloads"=>"downloads", "cpu"=>"cpu", "ram"=>"ram", "battery"=>"battery",
    "net"=>"network", "disk"=>"storage", "lang"=>"language-dark-aFiJF-sI",
    "cc"=>"quick-toggles", "chevron"=>"tray", "claude"=>"claude-plan-usage",
    "codex"=>"chatgpt-usage", "cursor"=>"cursor-usage", "grok"=>"grok-usage-dark-C-b1PNvn",
    "media"=>"now-playing", "weather"=>"weather", "stopwatch"=>"stopwatch",
    "notes"=>"notes", "dayprog"=>"day-progress", "monthprog"=>"month-progress",
    "yearprog"=>"year-progress", "theme"=>"theme-toggle", "brightness"=>"brightness",
    "recyclebin"=>"recycle-bin", "screenshot"=>"screenshot-capture", "recentshots"=>"recent-screenshots",
    "search"=>"search", "worldclock"=>"world-clock", "analog"=>"analog-classic",
    "flipclock"=>"flip-clock", "segclock"=>"segment-clock", "daynum"=>"day-number",
    "sun"=>"sunrise-and-sunset", "hourly"=>"hourly", "forecast"=>"weather-forecast",
    "metric"=>"metric", "currency"=>"currency", "screentime"=>"screen-time", _=>return None,
}) }

pub fn width(id:&str)->Option<f32> { reference(id)?; Some(match id {
    "battery"|"cpu"|"ram"|"analog"|"daynum"|"recyclebin"|"lang"|"chevron"|"clockweather"=>42.0,
    "theme"=>47.0, "clock"=>94.0, "clockwide"=>130.0,
    "pomodoro"|"focus"=>134.0, "stopwatch"=>132.0, "timer"=>126.0,
    "date"=>128.0, "git"=>114.0, "todo"|"notes"=>152.0,
    "downloads"=>146.0, "net"=>126.0, "disk"=>108.0,
    "claude"=>214.0, "codex"|"cursor"|"grok"=>194.0,
    "media"=>230.0, "weather"=>80.0, "cc"=>182.0,
    "dayprog"=>102.0, "monthprog"=>88.0, "yearprog"=>100.0,
    "brightness"=>152.0, "screenshot"=>96.0, "recentshots"=>130.0,
    "search"=>154.0, "worldclock"=>144.0, "flipclock"=>106.0,
    "segclock"=>114.0, "sun"=>128.0, "hourly"=>176.0, "forecast"=>152.0,
    "metric"=>136.0, "currency"=>150.0, "screentime"=>156.0, _=>return None,
}) }

struct Canvas<'a> { r:&'a mut Renderer, rc:RectF, sx:f32, sy:f32, w:f32 }
impl<'a> Canvas<'a> {
    fn new(r:&'a mut Renderer,rc:RectF,w:f32)->Self { Self{r,rc,sx:rc.w/w,sy:rc.h/42.0,w} }
    fn area(&self,x:f32,y:f32,w:f32,h:f32)->RectF { RectF{x:self.rc.x+x*self.sx,y:self.rc.y+y*self.sy,w:w.max(0.0)*self.sx,h:h.max(0.0)*self.sy} }
    fn scale(&self)->f32 {self.sx.min(self.sy)}
    fn fill(&mut self,x:f32,y:f32,w:f32,h:f32,rad:f32,color:u32)->Result<()> {self.r.fill_rounded_rect(self.area(x,y,w,h),rad*self.scale(),color)}
    fn gradient(&mut self,x:f32,y:f32,w:f32,h:f32,rad:f32,a:u32,b:u32)->Result<()> {self.r.fill_rounded_rect_gradient(self.area(x,y,w,h),rad*self.scale(),a,b)}
    fn stroke(&mut self,x:f32,y:f32,w:f32,h:f32,rad:f32,color:u32,thick:f32)->Result<()> {self.r.stroke_rounded_rect(self.area(x,y,w,h),rad*self.scale(),color,thick*self.scale())}
    fn text(&mut self,s:&str,x:f32,y:f32,w:f32,h:f32,px:f32,color:u32,align:Align,bold:bool)->Result<()> {
        self.r.draw_text_styled(s,self.area(x,y,w,h),px*self.scale(),color,align,if bold {TextStyle::Bold}else{TextStyle::Ui})
    }
    fn label(&mut self,s:&str,x:f32,y:f32,w:f32,h:f32,px:f32,color:u32)->Result<()> {self.text(s,x,y,w,h,px,color,Align::Left,false)}
    fn bold(&mut self,s:&str,x:f32,y:f32,w:f32,h:f32,px:f32,color:u32)->Result<()> {self.text(s,x,y,w,h,px,color,Align::Left,true)}
    fn centered(&mut self,s:&str,x:f32,y:f32,w:f32,h:f32,px:f32,color:u32,bold:bool)->Result<()> {self.text(s,x,y,w,h,px,color,Align::Center,bold)}
    fn icon(&mut self,s:&str,x:f32,y:f32,w:f32,h:f32,px:f32,color:u32)->Result<()> {self.r.draw_text_styled(s,self.area(x,y,w,h),px*self.scale(),color,Align::Center,TextStyle::Icon)}
    fn line(&mut self,x:f32,y:f32,xx:f32,yy:f32,color:u32,thick:f32)->Result<()> {self.r.stroke_line(self.rc.x+x*self.sx,self.rc.y+y*self.sy,self.rc.x+xx*self.sx,self.rc.y+yy*self.sy,color,thick*self.scale())}
    fn circle(&mut self,x:f32,y:f32,rad:f32,color:u32)->Result<()> {self.fill(x-rad,y-rad,rad*2.0,rad*2.0,rad,color)}
    fn arc(&mut self,x:f32,y:f32,rad:f32,start:f32,sweep:f32,color:u32,thick:f32)->Result<()> {
        self.r.stroke_arc(self.rc.x+x*self.sx,self.rc.y+y*self.sy,rad*self.scale(),start,sweep,color,thick*self.scale())
    }
    fn ring(&mut self,x:f32,y:f32,rad:f32,p:f32,color:u32,thick:f32)->Result<()> {
        self.arc(x,y,rad,-90.0,360.0,TRACK,thick)?;
        let p=p.clamp(0.0,1.0);
        if p>0.0 {
            self.arc(x,y,rad,-90.0,p*360.0,color,thick)?;
            for a in [-90.0,-90.0+p*360.0] {let a=a.to_radians();self.circle(x+rad*a.cos(),y+rad*a.sin(),thick/2.0,color)?;}
        }
        Ok(())
    }
    fn path(&mut self,d:&str,x:f32,y:f32,w:f32,h:f32,color:u32,stroke:Option<f32>)->Result<()> {svgpath::draw_path(self.r,d,self.area(x,y,w,h),24.0,color,stroke)}
    fn bar(&mut self,x:f32,y:f32,w:f32,h:f32,p:Option<f32>,color:u32)->Result<()> {
        self.fill(x,y,w,h,h/2.0,TRACK)?;
        if let Some(p)=p {if p>0.0 {self.fill(x,y,(w*p.clamp(0.0,1.0)).max(h).min(w),h,h/2.0,color)?;}}
        Ok(())
    }
    fn play(&mut self,x:f32,y:f32,rad:f32,color:u32,running:bool)->Result<()> {
        self.circle(x,y+0.7,rad+0.8,0x0000_0060)?;
        self.circle(x,y,rad,color)?;
        if running {
            for dx in [-2.2,1.0] {self.fill(x+dx,y-2.8,1.5,5.6,0.3,TEXT)?;}
        } else {self.path("M8 4 21 12 8 20Z",x-3.2,y-3.2,6.4,6.4,TEXT,None)?;}
        Ok(())
    }
}

pub fn draw(r:&mut Renderer,t:&TileView,rc:RectF,hover:bool)->Result<()> {
    if rc.w<2.0 || rc.h<2.0 {return Ok(());}
    let id=t.design_id.as_str();
    let w=width(id).unwrap_or(128.0);
    r.push_clip(rc)?;
    let result=(|| {
        let mut c=Canvas::new(r,rc,w);
        // Neutral dark glass with a quiet hairline, rather than a bright specular rim.
        if !matches!(id,"clock"|"segclock"|"search"|"lang"|"chevron"|"recentshots"|"screentime"|"git") {
            c.gradient(0.0,0.0,w,42.0,11.0,if hover {0x3030_33FC}else{0x2424_27F5},0x1C1C_1FF5)?;
            c.stroke(0.0,0.0,w,42.0,11.0,0xFFFF_FF1A,0.5)?;
        } else if hover { c.fill(0.0,0.0,w,42.0,10.0,0xFFFF_FF12)?; }
        match id {
            "cpu"|"ram"=>{
                let color=if id=="cpu" {CYAN}else{0x3484_FFFF};
                c.ring(21.0,21.0,14.0,t.progress.unwrap_or(0.0),color,2.4)?;
                c.centered(&t.title,9.0,13.0,24.0,7.0,6.0,MUTED,false)?;
                c.centered(&format!("{}%",t.value.trim_end_matches('%')),7.0,20.0,28.0,11.0,9.0,TEXT,true)?;
            }
            "battery"=>battery(&mut c,t)?,
            "pomodoro"|"focus"|"timer"|"stopwatch"=>counter(&mut c,t,id)?,
            "analog"=>analog(&mut c)?,
            "flipclock"=>flip(&mut c,t)?,
            "segclock"=>segments(&mut c,t)?,
            "daynum"=>{
                let now=Local::now();
                c.centered(&now.format("%a").to_string().to_uppercase(),3.0,6.0,w-6.0,13.0,7.0,RED,true)?;
                c.centered(&t.value,3.0,18.0,w-6.0,20.0,18.0,TEXT,true)?;
            }
            "clock"=>{
                c.centered(&t.value,0.0,3.0,w,25.0,21.0,TEXT,true)?;
                c.centered(&Local::now().format("%A, %b %d").to_string(),0.0,29.0,w,9.0,7.5,MUTED,false)?;
            }
            "clockweather"=>{
                c.r.with_rounded_clip(rc,11.0*c.scale(),|r| {
                    let mut a=Canvas::new(r,rc,42.0);
                    a.fill(0.0,0.0,42.0,21.0,0.0,0xFF3B_0AFF)?;
                    a.fill(0.0,21.0,21.0,21.0,0.0,0x0909_09FF)?;
                    a.fill(21.0,21.0,21.0,21.0,0.0,0xD7D7_DBFF)?;
                    a.centered(&t.value,2.0,2.0,38.0,18.0,12.0,0x0707_07FF,true)?;
                    let now=Local::now();a.centered(&now.format("%a").to_string(),1.0,23.0,19.0,7.0,5.3,MUTED,true)?;
                    a.centered(&now.day().to_string(),1.0,29.0,19.0,11.0,8.5,TEXT,true)?;
                    let temp=t.sub.split_whitespace().last().unwrap_or("—");a.circle(31.5,31.5,8.5,0xB9B9_BFFF)?;
                    a.centered(temp,22.0,23.0,19.0,17.0,8.5,0x2424_27FF,true)
                })?;
            }
            "date"|"clockwide"=>{
                let now=Local::now();
                c.bold(&now.format("%A").to_string().to_uppercase(),10.0,6.0,w-20.0,11.0,7.0,if id=="clockwide"{RED}else{TEXT})?;
                c.bold(&if id=="clockwide" {now.format("%B %d").to_string()}else{t.value.clone()},10.0,19.0,w-20.0,17.0,14.0,TEXT)?;
                if id=="clockwide" {c.text(&t.value,w-51.0,4.0,42.0,17.0,11.0,TEXT,Align::Right,true)?;c.text(&t.sub,w-51.0,25.0,42.0,10.0,8.0,MUTED,Align::Right,false)?;}
            }
            "dayprog"|"monthprog"|"yearprog"=>progress(&mut c,t,id)?,
            "net"=>network(&mut c,t)?,
            "disk"=>{
                c.icon("\u{EDA2}",7.0,5.0,12.0,12.0,11.0,RED)?;
                c.bold("Storage",23.0,5.0,w-30.0,10.0,8.0,TEXT)?;
                c.label(&format!("{} free / {}",t.value,t.detail),8.0,18.0,w-16.0,9.0,7.0,MUTED)?;
                c.bar(8.0,33.0,w-23.0,3.2,t.progress,RED)?;
            }
            "media"=>media(&mut c,t)?,
            "notes"|"todo"|"downloads"|"metric"|"screentime"=>icon_card(&mut c,t,id)?,
            "claude"|"codex"|"cursor"|"grok"=>usage(&mut c,t,id)?,
            "weather"=>{
                weather_icon(&mut c,&t.sub,9.0,10.0,23.0)?;
                let (temp,rest)=t.value.split_once(' ').unwrap_or((&t.value,""));
                c.bold(temp,37.0,7.0,w-43.0,19.0,16.0,TEXT)?;
                c.label(rest,37.0,27.0,w-42.0,9.0,6.8,MUTED)?;
            }
            "worldclock"=>world(&mut c,t)?,
            "hourly"|"forecast"=>forecast(&mut c,t,id)?,
            "sun"=>sun(&mut c,t)?,
            "theme"=>{
                let light=t.value.contains("Sáng");
                c.fill(6.0,12.0,w-12.0,18.0,9.0,0x0000_0040)?;
                let x=if light {w-15.0}else{15.0};
                c.circle(x,21.5,7.7,0x0000_0070)?;c.circle(x,20.5,7.5,TEXT)?;
                c.icon(if light {"\u{E706}"}else{"\u{E708}"},x-5.0,15.5,10.0,10.0,7.0,0x7777_7FFF)?;
            }
            "brightness"=>{
                c.icon("\u{E706}",8.0,5.0,12.0,12.0,11.0,TEXT)?;
                c.label("Brightness",24.0,5.0,85.0,12.0,8.0,TEXT)?;
                c.text(if t.progress.is_some(){&t.value}else{"N/A"},w-40.0,5.0,30.0,12.0,8.0,MUTED,Align::Right,false)?;
                c.bar(9.0,23.0,w-18.0,12.0,t.progress,0xFFFF_FFD0)?;
                if let Some(p)=t.progress {let x=15.0+(w-30.0)*p;c.circle(x,29.7,5.6,0x0000_0070)?;c.circle(x,29.0,5.3,TEXT)?;}
            }
            "recyclebin"=>{
                c.path("M6 7 7 20C9 22 15 22 17 20L18 7M4 6H20M9 6 10 3H14L15 6",14.0,14.0,14.0,14.0,0x9B9B_A0FF,Some(1.5))?;
                if !t.value.starts_with("0 ") {c.path("M9 8V18M12 8V18M15 8V18",14.0,14.0,14.0,14.0,0x9B9B_A0FF,Some(0.7))?;}
            }
            "screenshot"=>{
                for (i,color) in [0x569C_E1FF,0x3ED5_63FF,0xA88B_F5FF].iter().enumerate() {
                    let x=20.0+i as f32*28.0;c.circle(x,21.0,11.5,*color)?;
                    c.icon(["\u{E7C4}","\u{E7A8}","\u{E714}"][i],x-7.0,14.0,14.0,14.0,12.0,0xFFFF_FFB0)?;
                }
            }
            "recentshots"=>shots(&mut c)?,
            "search"=>{
                c.icon("\u{E721}",8.0,12.0,16.0,17.0,13.0,MUTED)?;
                c.label("Search…",31.0,12.0,w-38.0,17.0,12.0,0xFFFF_FF70)?;
            }
            "currency"=>{
                c.bold(&t.value,10.0,7.0,w-20.0,13.0,10.0,TEXT)?;
                c.icon("\u{E8AB}",9.0,23.0,12.0,12.0,9.0,MUTED)?;
                c.label(&t.sub,27.0,23.0,w-37.0,12.0,10.0,MUTED)?;
            }
            "lang"=>c.centered(&t.value,0.0,0.0,w,42.0,10.0,TEXT,true)?,
            "chevron"=>{
                let open=t.chevron.unwrap_or(false);let y=if open{19.0}else{23.0};
                c.line(18.0,y,21.0,if open{22.0}else{20.0},TEXT,1.5)?;
                c.line(21.0,if open{22.0}else{20.0},24.0,y,TEXT,1.5)?;
            }
            "cc"=>toggles(&mut c,t)?,
            "git"=>git(&mut c,t)?,
            _=>{}
        }
        Ok(())
    })();
    r.pop_clip()?;result
}

fn battery(c:&mut Canvas<'_>,t:&TileView)->Result<()> {
    let p=t.progress.unwrap_or(0.0).clamp(0.0,1.0);
    let water=if t.accent==0xFF5D_5DFF {0xEB45_45FF}else{BLUE};
    let top=if t.accent==0xFF5D_5DFF {0xFA80_70FF}else{0x4FCB_FFFF};
    let a=c.area(0.7,0.7,40.6,40.6);
    let level=a.y+a.h*(1.0-p);
    let amp=(a.h*0.025).min(a.h*p);
    if p>0.0 {
        c.r.with_rounded_clip(a,10.5*c.scale(),|r| {
            // Gradient body, topped by an independently constructed liquid surface.
            r.fill_rounded_rect_gradient(RectF{x:a.x,y:level,w:a.w,h:(a.y+a.h-level).max(0.1)},0.0,top,water)?;
            use windows::Win32::Graphics::Direct2D::Common::{D2D_POINT_2F,D2D1_FIGURE_BEGIN_FILLED,D2D1_FIGURE_END_CLOSED};
            r.draw_geometry_with(|sink| {
                unsafe {
                    sink.BeginFigure(D2D_POINT_2F{x:a.x,y:level},D2D1_FIGURE_BEGIN_FILLED);
                    for i in 1..=48 {let x=i as f32/48.0;let y=level+(x*std::f32::consts::TAU*1.5).sin()*amp;sink.AddLine(D2D_POINT_2F{x:a.x+x*a.w,y});}
                    sink.AddLine(D2D_POINT_2F{x:a.x+a.w,y:level+amp+0.2});
                    sink.AddLine(D2D_POINT_2F{x:a.x,y:level+amp+0.2});
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                }Ok(())
            },top,None)
        })?;
    }
    c.stroke(0.7,0.7,40.6,40.6,10.5,0xFFFF_FF70,0.4)?;
    if t.title.contains("ĐANG SẠC") {c.path("M13 1 4 14H11L10 23 20 10H13Z",10.0,16.0,8.0,10.0,TEXT,None)?;}
    let value=if t.progress.is_some(){t.value.trim_end_matches('%')}else{"—"};
    c.centered(value,if t.title.contains("ĐANG SẠC"){19.0}else{6.0},13.0,if t.title.contains("ĐANG SẠC"){16.0}else{30.0},16.0,10.0,TEXT,true)?;
    if t.progress.is_none() {c.centered("No battery",3.0,30.0,36.0,7.0,5.5,MUTED,false)?;}
    Ok(())
}

fn counter(c:&mut Canvas<'_>,t:&TileView,id:&str)->Result<()> {
    let color=match id {"stopwatch"=>ORANGE,"timer"=>BLUE,_=>RED};
    let label=match id {"stopwatch"=>"Stopwatch","timer"=>"Timer",_=>if t.title.contains("NGHỈ"){"Break"}else{"Focus"}};
    if id!="stopwatch" {c.stroke(1.0,1.0,c.w-2.0,40.0,10.5,color,1.6)?;}
    c.bold(label,11.0,6.0,c.w-52.0,11.0,8.0,color)?;
    c.bold(&t.value,11.0,19.0,c.w-49.0,17.0,15.0,TEXT)?;
    let running=match id {"pomodoro"=>crate::data::productivity::pomodoro().3,"focus"=>crate::data::focus::snapshot().running,"timer"=>crate::data::timer::snapshot().1,_=>crate::data::productivity::watch().1};
    c.play(c.w-21.0,21.0,11.5,color,running)?;
    if id=="pomodoro" {c.text("»",c.w-8.0,8.0,6.0,26.0,9.0,MUTED,Align::Center,false)?;}
    Ok(())
}

fn analog(c:&mut Canvas<'_>)->Result<()> {
    let (cx,cy,rad)=(21.0,21.0,13.0);
    c.circle(cx,cy,rad+0.8,0x4848_4BFF)?;c.circle(cx,cy,rad,0xF8F8_F8FF)?;
    for i in 0..60 {let a=(i as f32*6.0-90.0).to_radians();let inner=if i%5==0{rad-1.6}else{rad-0.7};c.line(cx+a.cos()*inner,cy+a.sin()*inner,cx+a.cos()*(rad-0.3),cy+a.sin()*(rad-0.3),0x3333_33FF,if i%5==0{0.5}else{0.25})?;}
    for (s,x,y) in [("12",17.0,10.3),("3",27.0,18.2),("6",17.4,26.7),("9",9.3,18.2)] {c.centered(s,x,y,7.0,6.0,3.1,0x1919_19FF,true)?;}
    let now=Local::now();
    for (angle,len,color,width) in [(now.hour() as f32*30.0+now.minute() as f32/2.0,7.2,0x3030_33FF,0.8),(now.minute() as f32*6.0,10.0,0x3B82_F6FF,0.8),(now.second() as f32*6.0,10.8,0xF060_60FF,0.35)] {
        let a=(angle-90.0).to_radians();c.line(cx-a.cos()*1.5,cy-a.sin()*1.5,cx+a.cos()*len,cy+a.sin()*len,color,width)?;
    }
    c.circle(cx,cy,0.9,0x2424_27FF)
}

fn flip(c:&mut Canvas<'_>,t:&TileView)->Result<()> {
    for (i,ch) in t.value.chars().filter(|c|c.is_ascii_digit()).take(4).enumerate() {
        let x=6.0+i as f32*19.0+if i>=2 {4.0}else{0.0};
        c.gradient(x,6.5,18.5,29.5,5.2,0x3535_38FF,0x2020_23FF)?;
        c.stroke(x,6.5,18.5,29.5,5.2,0xFFFF_FF20,0.4)?;
        c.centered(&ch.to_string(),x,7.0,18.5,29.0,19.0,TEXT,true)?;
        c.line(x,21.0,x+18.5,21.0,0x0000_00A0,0.5)?;
    }
    c.label(&Local::now().format("%p").to_string(),91.0,28.0,13.0,8.0,5.0,MUTED)
}

fn segments(c:&mut Canvas<'_>,t:&TileView)->Result<()> {
    const MASK:[u8;10]=[0x3f,0x06,0x5b,0x4f,0x66,0x6d,0x7d,0x07,0x7f,0x6f];
    let weekday=Local::now().format("%A").to_string().to_uppercase();
    c.stroke(4.0,4.0,28.0,7.0,3.5,RED,0.8)?;c.centered(&weekday,4.0,4.0,28.0,7.0,4.8,RED,true)?;
    for (i,ch) in t.value.chars().filter(|c|c.is_ascii_digit()).take(4).enumerate() {
        let x=4.0+i as f32*27.5;
        let mask=MASK[ch.to_digit(10).unwrap() as usize];
        for (j,(dx,dy,w,h)) in [(2.5,0.0,18.0,2.4),(21.0,3.2,2.4,8.0),(21.0,13.8,2.4,8.0),(2.5,22.4,18.0,2.4),(0.0,13.8,2.4,8.0),(0.0,3.2,2.4,8.0),(2.5,11.2,18.0,2.4)].iter().enumerate() {
            c.fill(x+dx,13.0+dy,*w,*h,0.8,if mask&(1<<j)!=0 {TEXT}else{0xFFFF_FF0A})?;
        }
    }Ok(())
}

fn progress(c:&mut Canvas<'_>,t:&TileView,id:&str)->Result<()> {
    let now=Local::now();let p=t.progress.unwrap_or(0.0);
    let label=match id {"dayprog"=>"Day progress","monthprog"=>"Month progress",_=>"Year progress"};
    c.bold(label,8.0,5.0,c.w-16.0,10.0,7.0,TEXT)?;
    c.text(&format!("{:.0}%",p*100.0),c.w-34.0,5.0,26.0,10.0,7.0,MUTED,Align::Right,false)?;
    let (count,cols)=match id {"dayprog"=>(24,24),"monthprog"=>{let d=if now.month()==12 {chrono::NaiveDate::from_ymd_opt(now.year()+1,1,1)}else{chrono::NaiveDate::from_ymd_opt(now.year(),now.month()+1,1)}.unwrap().pred_opt().unwrap().day() as usize;(d,16)},_=>(t.cells.len(),53)};
    let rows=count.div_ceil(cols).max(1);let pitch=(c.w-16.0)/cols as f32;
    let size=(pitch-0.8).min((20.0/rows as f32)-0.5).max(0.4);
    for i in 0..count {
        let done=if id=="yearprog" {t.cells[i]>0}else{(i as f32)<p*count as f32};
        let (col,row)=if id=="yearprog" {(i/7,i%7)}else{(i%cols,i/cols)};
        c.fill(8.0+col as f32*pitch,18.0+row as f32*(if id=="yearprog"{2.65}else{9.0}),size,size,size/2.0,if done{0xEEEE_F0FF}else{0xFFFF_FF22})?;
    }Ok(())
}

fn network(c:&mut Canvas<'_>,t:&TileView)->Result<()> {
    c.bold("↓",10.0,9.0,9.0,11.0,10.0,CYAN)?;
    c.label(t.value.trim_start_matches('↓').trim(),21.0,9.0,48.0,11.0,8.0,TEXT)?;
    c.bold("↑",c.w-18.0,9.0,9.0,11.0,10.0,0x9B7E_FFFF)?;
    c.text(t.sub.trim_start_matches('↑').trim(),70.0,9.0,c.w-91.0,11.0,8.0,TEXT,Align::Right,false)?;
    let peak=t.series.iter().flatten().copied().fold(1.0f32,f32::max);
    for (series,color) in t.series.iter().zip([CYAN,0x9B7E_FFFF]) {
        for (i,pair) in series.windows(2).enumerate() {
            let pitch=(c.w-20.0)/47.0;let offset=48usize.saturating_sub(series.len());
            c.line(10.0+(offset+i) as f32*pitch,31.0-pair[0]/peak*10.0,10.0+(offset+i+1) as f32*pitch,31.0-pair[1]/peak*10.0,color,1.2)?;
        }
    }Ok(())
}

fn media(c:&mut Canvas<'_>,t:&TileView)->Result<()> {
    // Original vector record sleeve, also used when the media session supplies no artwork.
    c.gradient(6.0,6.0,31.0,31.0,7.0,0x7A7A_80FF,0x1414_17FF)?;
    c.circle(17.0,15.0,6.0,0xFFFF_FF12)?;
    c.arc(21.5,21.5,7.0,0.0,360.0,0xFFFF_FF70,0.3)?;c.circle(21.5,21.5,0.8,TEXT)?;
    c.bold(&t.title,43.0,8.0,c.w-105.0,13.0,10.0,TEXT)?;
    c.label(&t.sub,43.0,23.0,c.w-105.0,11.0,7.5,MUTED)?;
    c.icon("\u{E892}",c.w-57.0,14.0,10.0,14.0,8.0,TEXT)?;
    c.play(c.w-32.0,21.0,11.5,BLUE,t.progress.is_some())?;
    c.icon("\u{E893}",c.w-17.0,14.0,10.0,14.0,8.0,TEXT)?;
    // A progress line is shown only when a real session timeline is available.
    if let Some(p)=t.mini.first().map(|p|p.0) {c.bar(6.0,39.0,c.w-12.0,1.8,Some(p),BLUE)?;}
    Ok(())
}

fn icon_card(c:&mut Canvas<'_>,t:&TileView,id:&str)->Result<()> {
    match id {
        "notes"=>{
            c.gradient(7.0,7.0,29.0,29.0,6.5,0xFAFA_F6FF,0xD7D5_CEFF)?;
            c.r.fill_top_rounded_rect(c.area(7.0,7.0,29.0,8.0),6.5*c.scale(),0xFFCF_24FF)?;
            for y in [16.0,21.0,27.0,32.0] {c.line(8.0,y,35.0,y,0x8888_8840,0.3)?;}
        }
        "todo"=>{
            c.ring(21.0,21.0,10.0,t.progress.unwrap_or(0.0),BLUE,2.3)?;
            c.centered(&t.value,11.0,15.0,20.0,12.0,8.0,TEXT,true)?;
        }
        "downloads"=>{
            c.gradient(7.0,7.0,28.0,28.0,7.0,0x4AB6_FFFF,BLUE)?;
            c.line(21.0,16.0,21.0,25.0,TEXT,1.7)?;c.line(17.0,22.0,21.0,26.0,TEXT,1.7)?;c.line(21.0,26.0,25.0,22.0,TEXT,1.7)?;
        }
        "metric"=>{c.fill(7.0,7.0,28.0,28.0,8.0,0x0909_09FF)?;c.centered("₿",7.0,7.0,28.0,28.0,18.0,TEXT,false)?;}
        "screentime"=>{
            c.fill(14.0,11.0,11.0,20.0,3.0,BLUE)?;c.fill(12.0,14.0,15.0,14.0,4.0,BLUE)?;
            c.line(19.5,17.0,19.5,21.0,0x0873_D5FF,1.0)?;c.line(19.5,21.0,22.0,23.0,0x0873_D5FF,1.0)?;
        }
        _=>{}
    }
    let title=match id {"notes"=>"Notes","todo"=>"To-dos","downloads"=>"Downloads","metric"=>"Bitcoin",_=>"Screen Time"};
    c.bold(title,43.0,7.0,if id=="metric"{38.0}else{c.w-51.0},11.0,8.5,TEXT)?;
    if id=="metric" {
        let color=if t.accent==0 {MUTED}else{t.accent};
        c.fill(c.w-42.0,8.0,34.0,10.0,5.0,(color&0xFFFF_FF00)|0x35)?;
        c.centered(&t.detail,c.w-42.0,8.0,34.0,10.0,6.3,color,true)?;
    }
    let body=match id {"notes"|"todo"=>&t.sub,_=>&t.value};
    c.label(body,43.0,21.0,c.w-if id=="screentime"{84.0}else{51.0},13.0,if id=="metric"{12.0}else{8.0},if id=="metric"{TEXT}else{MUTED})?;
    if id=="screentime" {
        let values=t.series.first().map(Vec::as_slice).unwrap_or(&[]);let peak=values.iter().copied().fold(1.0f32,f32::max);
        for (i,v) in values.iter().take(6).enumerate() {let h=(v/peak*13.0).max(0.5);c.fill(c.w-37.0+i as f32*3.5,31.0-h,2.0,h,0.6,BLUE)?;}
    }
    Ok(())
}

fn usage(c:&mut Canvas<'_>,t:&TileView,id:&str)->Result<()> {
    let plate=if id=="claude" {0xD17B_58FF}else{0x0909_09FF};
    c.fill(6.0,6.0,31.0,31.0,7.5,plate)?;
    if let Some(icon)=&t.icon {c.r.draw_bitmap_rounded(icon.w,icon.h,&icon.rgba,c.area(6.0,6.0,31.0,31.0),7.5*c.scale())?;}
    else if id=="cursor" {c.path("M12 2 21 7 12 12 3 7Z M3 7 12 12V22L3 17Z",9.0,9.0,25.0,25.0,TEXT,None)?;c.path("M12 12 21 7V17L12 22Z",9.0,9.0,25.0,25.0,0x8888_88FF,None)?;}
    else if let Some((path,stroke))=t.glyph {c.path(path,9.0,9.0,25.0,25.0,TEXT,stroke)?;}
    let n=t.rows.len().max(1).min(3);
    if t.rows.is_empty() {
        c.bold(&t.title,43.0,7.0,c.w-51.0,11.0,8.0,TEXT)?;
        c.label(&t.value,43.0,20.0,c.w-51.0,12.0,9.0,MUTED)?;
        if t.progress.is_some() {c.bar(43.0,33.0,c.w-50.0,3.0,t.progress,GREEN)?;}
    } else {
        let top=21.0-n as f32*5.5;
        for (i,row) in t.rows.iter().take(3).enumerate() {
            let y=top+i as f32*11.0;
            c.bold(&row.label,43.0,y-4.0,43.0,9.0,7.5,TEXT)?;
            c.bar(90.0,y,c.w-154.0,2.7,Some(row.frac),row.color)?;
            c.text(&row.right,c.w-60.0,y-4.0,30.0,9.0,6.5,MUTED,Align::Right,false)?;
            c.text(&row.value,c.w-27.0,y-4.0,20.0,9.0,7.5,TEXT,Align::Right,true)?;
        }
    }Ok(())
}

fn weather_icon(c:&mut Canvas<'_>,code:&str,x:f32,y:f32,size:f32)->Result<()> {
    let code=code.parse::<u32>().unwrap_or(0);
    if code<=2 {
        let (cx,cy)=(x+size*0.5,y+size*0.5);let radius=size*0.22;
        for i in 0..8 {let a=i as f32*std::f32::consts::FRAC_PI_4;c.line(cx+a.cos()*size*0.34,cy+a.sin()*size*0.34,cx+a.cos()*size*0.45,cy+a.sin()*size*0.45,0xFFCC_00FF,size*0.055)?;}
        c.circle(cx,cy,radius,0xFFD4_29FF)?;
    }
    if code>0 {
        let color=if code>=49{0x92A5_BAFF}else{0xDFE4_EBFF};
        c.circle(x+size*0.40,y+size*0.60,size*0.18,color)?;c.circle(x+size*0.59,y+size*0.51,size*0.23,color)?;c.circle(x+size*0.75,y+size*0.64,size*0.15,color)?;
        c.fill(x+size*0.35,y+size*0.63,size*0.4,size*0.16,size*0.04,color)?;
        if code>=49 {for dx in [0.4,0.6,0.8] {c.line(x+size*dx,y+size*0.85,x+size*(dx-0.06),y+size*0.96,BLUE,0.6)?;}}
    }Ok(())
}

fn world(c:&mut Canvas<'_>,t:&TileView)->Result<()> {
    for (i,row) in t.rows.iter().take(3).enumerate() {
        let y=5.0+i as f32*11.0;
        let hour=row.value.split(':').next().and_then(|n|n.parse::<u32>().ok()).unwrap_or(12);
        if (6..18).contains(&hour) {c.circle(13.0,y+5.0,2.2,0xFFC4_00FF)?;}else{c.circle(13.0,y+5.0,2.5,0xA8B8_FFFF)?;c.circle(14.3,y+3.5,2.0,0x2424_27FF)?;}
        c.label(&row.label,23.0,y,c.w-78.0,10.0,8.0,TEXT)?;
        c.text(&row.value,c.w-50.0,y,38.0,10.0,8.8,TEXT,Align::Right,true)?;
    }Ok(())
}

fn forecast(c:&mut Canvas<'_>,t:&TileView,id:&str)->Result<()> {
    if t.rows.is_empty() {return c.centered(&t.value,6.0,0.0,c.w-12.0,42.0,9.0,MUTED,false);}
    let pitch=(c.w-12.0)/t.rows.len() as f32;
    for (i,row) in t.rows.iter().enumerate() {
        let x=6.0+i as f32*pitch;
        c.centered(&row.label,x,4.0,pitch,8.0,6.3,MUTED,false)?;
        weather_icon(c,&row.right,x+(pitch-15.0)/2.0,13.0,15.0)?;
        c.centered(&row.value,x,29.0,pitch,10.0,7.3,TEXT,id=="hourly")?;
    }Ok(())
}

fn sun(c:&mut Canvas<'_>,t:&TileView)->Result<()> {
    // Low, wide solar trajectory with the current sun on the actual daylight fraction.
    let p=t.progress.unwrap_or(0.0);let mut prev=None;
    for i in 0..=48 {let x=i as f32/48.0;let y=20.0-(x*std::f32::consts::PI).sin()*7.0;
        if let Some((px,py))=prev {c.line(px,py,4.0+x*(c.w-8.0),y,0xFFB3_4AFF,0.9)?;}
        prev=Some((4.0+x*(c.w-8.0),y));
    }
    if t.progress.is_some() {let x=4.0+p*(c.w-8.0);let y=20.0-(p*std::f32::consts::PI).sin()*7.0;for rad in [5.0,4.0,3.0] {c.circle(x,y,rad,0xFFD3_2520)?;}c.circle(x,y,2.3,0xFFCE_24FF)?;}
    c.icon("\u{E706}",4.0,25.0,11.0,11.0,8.0,ORANGE)?;
    c.icon("\u{E706}",c.w-15.0,25.0,11.0,11.0,8.0,ORANGE)?;
    c.label(&t.value,18.0,29.0,c.w-36.0,9.0,7.2,TEXT)
}

fn shots(c:&mut Canvas<'_>)->Result<()> {
    let shots=crate::win::widget_tools::shots();
    if shots.is_empty(){return c.centered("Chưa có ảnh chụp",5.0,0.0,c.w-10.0,42.0,9.0,MUTED,false);}
    let pitch=(c.w-12.0)/3.0;
    for (i,shot) in shots.iter().take(3).enumerate() {
        let a=c.area(6.0+i as f32*pitch,6.0,pitch-3.0,30.0);
        c.r.draw_bitmap_rounded(shot.w,shot.h,&shot.rgba,a,6.0*c.scale())?;
    }Ok(())
}

fn toggles(c:&mut Canvas<'_>,t:&TileView)->Result<()> {
    if let Some(st)=t.status {
        c.fill(6.0,7.0,52.0,28.0,14.0,0x319C_FFFF)?;
        super::sficons::draw_wifi(c.r,c.area(15.0,15.0,12.0,12.0),st.wifi,0x0875_D3FF)?;
        c.line(32.0,14.0,32.0,28.0,0x0875_D380,0.5)?;c.path("M9 5 16 12 9 19",40.0,17.0,8.0,8.0,0x0875_D3FF,Some(2.0))?;
        super::sficons::draw_speaker(c.r,c.area(68.0,15.0,15.0,12.0),st.volume,st.muted,TEXT)?;
        c.bar(90.0,18.0,80.0,6.0,Some(st.volume),TEXT)?;
    }Ok(())
}

fn git(c:&mut Canvas<'_>,t:&TileView)->Result<()> {
    c.bold(&t.title,6.0,3.0,53.0,10.0,7.0,TEXT)?;
    c.text(&t.value,59.0,3.0,c.w-65.0,10.0,6.0,MUTED,Align::Right,false)?;
    let rows=3;let cols=t.cells.len().div_ceil(rows).max(1);let pitch=(c.w-12.0)/cols as f32;
    for (i,level) in t.cells.iter().enumerate() {
        let color=[0xFFFF_FF0D,0x104C_30FF,0x0072_3AFF,0x26A6_41FF,0x40D4_59FF][(*level as usize).min(4)];
        c.fill(6.0+(i/rows) as f32*pitch,17.0+(i%rows) as f32*7.0,pitch-1.2,6.0,1.5,color)?;
    }Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{control_center::ControlState,tiles::DataState,gallery};
    use crate::model::*;
    use crate::settings::{Settings,WIDGET_IDS};

    #[test]
    fn renders_reference_widgets_and_gallery_at_two_dpi_scales() {
        let now=chrono::Utc::now();
        let mut data=DataState::default();
        data.widgets=WidgetStats{battery:Some(Battery{percent:Some(86),charging:true,plugged_in:true}),net:Some((834000.0,32000.0)),disk_free:[Some(120*1024_u64.pow(3)),None],disk_total:[Some(512*1024_u64.pow(3)),None],gpu_pct:None};
        data.media=Some(Media{title:"Midnight City".into(),artist:"M83".into(),playing:true,timeline:Some((70.0,244.0))});
        data.git=Some(crate::data::git::GitSummary{today:3,week:18,streak:7,heatmap:(0..119).map(|i|GitDay{date:String::new(),commits:(i%8) as u32}).collect(),error:None});
        data.net_history=(0..48).map(|i|((i as f32*0.2).sin().abs()*100.0,(i as f32*0.3).sin().abs()*20.0)).collect();
        data.ai=["Claude","Codex","Cursor","Grok"].iter().map(|name|AiUsage{name:(*name).into(),session_pct:35.0,windows:vec![AiWindow{label:"Session".into(),used_pct:35.0,reset_text:"1h 25m".into()},AiWindow{label:"Week".into(),used_pct:18.0,reset_text:"Wed".into()}],..Default::default()}).collect();
        data.weather=Some(crate::data::weather::Weather{location:"Hà Nội".into(),current:crate::data::weather::Current{temp:28.0,feels:30.0,code:0,humidity:60.0,wind:8.0},daily:(0..5).map(|i|crate::data::weather::Day{date:(now.date_naive()+chrono::Duration::days(i)).to_string(),min:23.0,max:30.0,code:2}).collect(),hourly:(1..=6).map(|i|crate::data::weather::Hour{at:now.timestamp()+i*3600,temp:27.0,code:3}).collect(),sunrise:Some(now.timestamp()-3600),sunset:Some(now.timestamp()+3600),utc_offset_seconds:0,fetched_at:now});
        let settings=Settings{note:"Ideas for tomorrow\nRead, build, repeat.".into(),..Default::default()};
        let control=ControlState{volume:0.65,wifi:Some(true),wifi_signal:Some(90),..Default::default()};
        let stats=Stats{cpu_pct:8.0,ram_used_mb:10500,ram_total_mb:16000};
        let mut views=Vec::new();
        for id in WIDGET_IDS {
            let view=crate::tile_view(id,&data,&control,&stats,0.5,&settings);
            assert_eq!(reference(id).is_some(),id!="gpu","reference mapping: {id}");
            views.push((id,view));
        }
        std::fs::create_dir_all("target/visual-audit").unwrap();
        for dpi in [96,192] {
            let (mut r,bitmap)=Renderer::test_surface(1040*dpi/96,1080*dpi/96,dpi).unwrap();
            r.begin().unwrap();r.fill_rect(RectF{x:0.0,y:0.0,w:1040.0,h:1080.0},0x1414_16FF).unwrap();
            for (i,(id,v)) in views.iter().enumerate() {
                let (x,y)=(8.0+(i%4) as f32*260.0,8.0+(i/4) as f32*90.0);
                r.draw_text(id,RectF{x,y,w:246.0,h:12.0},9.0,MUTED,Align::Left).unwrap();
                let w=width(&v.design_id).unwrap_or(64.0);
                for (off,factor) in [(14.0,1.0),(60.0,0.65)] {
                    super::super::dock::draw_tile(&mut r,v,RectF{x,y:y+off,w:w*factor,h:if factor<1.0{22.0}else{42.0}},false).unwrap();
                }
            }
            for (i,id) in ["clockwide","clockweather"].iter().enumerate() {
                let mut v=crate::tile_view("clock",&data,&control,&stats,0.0,&settings);
                v.design_id=(*id).into();v.sub="☀ 28°".into();
                let x=268.0+i as f32*260.0;
                r.draw_text(id,RectF{x,y:998.0,w:246.0,h:12.0},9.0,MUTED,Align::Left).unwrap();
                draw(&mut r,&v,RectF{x,y:1012.0,w:width(id).unwrap(),h:42.0},false).unwrap();
            }
            r.end().unwrap();Renderer::save_test_png(&bitmap,std::path::Path::new(&format!("target/visual-audit/widgets-{dpi}.png"))).unwrap();
        }
        let entries:Vec<_>=views.into_iter().map(|(id,view)|gallery::Entry{id,name:crate::settings::widget_label(id),cat:gallery::ITEMS.iter().find(|p|p.0==id).unwrap().1,view,width:crate::configured_widget_width(id,&settings),on:true,fixed:false,disabled:false}).collect();
        for cat in 0..gallery::CATS.len() {
            gallery::reset();gallery::select(cat);
            let (mut r,bitmap)=Renderer::test_surface(1440,940,192).unwrap();
            r.begin().unwrap();r.fill_rect(RectF{x:0.0,y:0.0,w:720.0,h:470.0},0x1919_1CFF).unwrap();
            gallery::draw(&mut r,RectF{x:0.0,y:0.0,w:720.0,h:470.0},&entries).unwrap();r.end().unwrap();
            Renderer::save_test_png(&bitmap,std::path::Path::new(&format!("target/visual-audit/gallery-{cat}.png"))).unwrap();
        }
        // Low/empty/full battery masks and short/network-unavailable series must all render.
        let (mut r,bitmap)=Renderer::test_surface(800,100,96).unwrap();r.begin().unwrap();
        for (i,p) in [None,Some(0.0),Some(0.01),Some(0.2),Some(0.86),Some(1.0)].into_iter().enumerate() {
            let mut v=TileView::card("PIN · ĐANG SẠC","1%");v.design_id="battery".into();v.progress=p;
            draw(&mut r,&v,RectF{x:i as f32*50.0,y:0.0,w:42.0,h:42.0},false).unwrap();
        }r.end().unwrap();
        Renderer::save_test_png(&bitmap,std::path::Path::new("target/visual-audit/battery-states.png")).unwrap();
    }

    #[test]
    fn network_history_is_bounded_and_does_not_bridge_missing_samples() {
        use super::super::tiles::DataUpdate;
        let mut d=DataState::default();
        for i in 0..60 {d.apply(DataUpdate::Widgets(WidgetStats{net:Some((i as f64,2.0)),..Default::default()}));}
        assert_eq!(d.net_history.len(),48);assert_eq!(d.net_history[0],(12.0,2.0));
        d.apply(DataUpdate::Widgets(WidgetStats::default()));assert!(d.net_history.is_empty());
    }
}

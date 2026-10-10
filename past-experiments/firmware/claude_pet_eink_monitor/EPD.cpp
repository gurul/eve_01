// Vendored from Elecrow's CrowPanel 4.2" demo (4.2_partial_refresh), see
// this folder's README.md. Local changes:
//  - EPD_ReadBusy gets a 8s escape so a wedged panel can't hang loop()
//    forever — a full SSD1683 refresh is ~4s worst case, so 8s of BUSY means
//    the panel is gone, not slow.
//  - Every display path now maintains the controller's "old data" RAM plane
//    (0x26). The stock driver only ever writes new frames to 0x24, so a
//    partial update diffs against whatever stale frame 0x26 held and black
//    pixels from earlier frames survive — on-screen text visibly overlaps
//    after two different screens. Partial updates write 0x24, refresh, then
//    copy the same frame into 0x26 so the *next* partial diffs correctly;
//    full updates write both planes up front.
#include "EPD.h"

static void EPD_WritePlane(uint8_t reg, const uint8_t *Image)
{
  uint16_t i, j, Width, Height;
  Width = (EPD_W % 8 == 0) ? (EPD_W / 8) : (EPD_W / 8 + 1);
  Height = EPD_H;
  EPD_WR_REG(reg);
  for (j = 0; j < Height; j++)
  {
    for (i = 0; i < Width; i++)
    {
      EPD_WR_DATA8(Image[i + j * Width]);
    }
  }
}

void EPD_ReadBusy(void)
{
  uint32_t start = millis();
  while (EPD_ReadBUSY != 0)
  {
    if (millis() - start > 8000) break;
    delay(1);
  }
}

void EPD_RESET(void)
{
  EPD_RES_Set();
  delay(100);
  EPD_RES_Clr();
  delay(10);
  EPD_RES_Set();
  delay(10);
}

void EPD_Sleep(void)
{
  // Power the analog stage down properly before deep sleep (GxEPD2's
  // _PowerOff). Entering 0x10 deep sleep with the booster still up is out of
  // spec order and is a prime suspect for RAM-plane corruption across sleep
  // — the overlapping-text bug.
  EPD_WR_REG(0x22);
  EPD_WR_DATA8(0x83);
  EPD_WR_REG(0x20);
  EPD_ReadBusy();
  EPD_WR_REG(0x10);
  EPD_WR_DATA8(0x01);
  delay(50);
}

// Wake from deep sleep for a partial refresh: hardware reset + soft reset +
// internal temperature sensor select. Mirrors GxEPD2's _InitDisplay — the
// full EPD_Init() is NOT wanted here.
void EPD_Wake(void)
{
  EPD_RESET();
  EPD_ReadBusy();
  EPD_WR_REG(0x12);
  EPD_ReadBusy();
  EPD_WR_REG(0x18);   // temperature sensor: internal
  EPD_WR_DATA8(0x80);
}


void EPD_Update(void)
{
  EPD_WR_REG(0x22);
  EPD_WR_DATA8(0xF7);
  EPD_WR_REG(0x20);
  EPD_ReadBusy();
}
void EPD_Update_Fast(void)
{
  EPD_WR_REG(0x22);
  EPD_WR_DATA8(0xC7);
  EPD_WR_REG(0x20);
  EPD_ReadBusy();
}

void EPD_Update_Part(void)
{
  // 0xFC, not the stock 0xFF — the differential (display mode 2) update
  // value GxEPD2 uses on this exact panel. 0x21 is re-issued here so the
  // old-data plane is never bypassed regardless of what init ran before.
  EPD_WR_REG(0x21);
  EPD_WR_DATA8(0x00);
  EPD_WR_DATA8(0x00);
  EPD_WR_REG(0x22);
  EPD_WR_DATA8(0xFC);
  EPD_WR_REG(0x20);
  EPD_ReadBusy();
}


void EPD_Address_Set(uint16_t xs, uint16_t ys, uint16_t xe, uint16_t ye)
{
  EPD_WR_REG(0x44); // SET_RAM_X_ADDRESS_START_END_POSITION
  EPD_WR_DATA8((xs >> 3) & 0xFF);
  EPD_WR_DATA8((xe >> 3) & 0xFF);

  EPD_WR_REG(0x45); // SET_RAM_Y_ADDRESS_START_END_POSITION
  EPD_WR_DATA8(ys & 0xFF);
  EPD_WR_DATA8((ys >> 8) & 0xFF);
  EPD_WR_DATA8(ye & 0xFF);
  EPD_WR_DATA8((ye >> 8) & 0xFF);
}


void EPD_SetCursor(uint16_t xs, uint16_t ys)
{
  EPD_WR_REG(0x4E); // SET_RAM_X_ADDRESS_COUNTER
  EPD_WR_DATA8(xs & 0xFF);

  EPD_WR_REG(0x4F); // SET_RAM_Y_ADDRESS_COUNTER
  EPD_WR_DATA8(ys & 0xFF);
  EPD_WR_DATA8((ys >> 8) & 0xFF);
}


void EPD_Init(void)
{
  EPD_RESET();
  EPD_ReadBusy();
  EPD_WR_REG(0x12);   // soft  reset
  EPD_ReadBusy();
  EPD_WR_REG(0x21); //  Display update control
  EPD_WR_DATA8(0x40);
  EPD_WR_DATA8(0x00);
  EPD_WR_REG(0x3C); //BorderWavefrom
  EPD_WR_DATA8(0x05);
  EPD_WR_REG(0x11); // data  entry  mode
  EPD_WR_DATA8(0x03);   // X-mode
  EPD_Address_Set(0, 0, EPD_W - 1, EPD_H - 1);
  EPD_SetCursor(0, 0);
  EPD_ReadBusy();
}

void EPD_Init_Fast(uint8_t mode)
{
  EPD_RESET();
  EPD_ReadBusy();
  EPD_WR_REG(0x12);   // soft  reset
  EPD_ReadBusy();
  EPD_WR_REG(0x21);
  EPD_WR_DATA8(0x40);
  EPD_WR_DATA8(0x00);
  EPD_WR_REG(0x3C);
  EPD_WR_DATA8(0x05);
  if (mode == Fast_Seconds_1_5s)
  {
    EPD_WR_REG(0x1A); // Write to temperature register
    EPD_WR_DATA8(0x6E);
  }
  else if (mode == Fast_Seconds_1_s)
  {
    EPD_WR_REG(0x1A); // Write to temperature register
    EPD_WR_DATA8(0x5A);
  }
  EPD_WR_REG(0x22); // Load temperature value
  EPD_WR_DATA8(0x91);
  EPD_WR_REG(0x20);
  EPD_ReadBusy();
  EPD_WR_REG(0x11); // data  entry  mode
  EPD_WR_DATA8(0x03);   // X-mode
  EPD_Address_Set(0, 0, EPD_W - 1, EPD_H - 1);
  EPD_SetCursor(0, 0);
  EPD_ReadBusy();
}


void EPD_Clear(void)
{
  uint16_t i, j, Width, Height;
  Width = (EPD_W % 8 == 0) ? (EPD_W / 8) : (EPD_W / 8 + 1);
  Height = EPD_H;
  EPD_Init();
  EPD_WR_REG(0x24);
  for (j = 0; j < Height; j++)
  {
    for (i = 0; i < Width; i++)
    {
      EPD_WR_DATA8(0xFF);
    }
  }

  EPD_WR_REG(0x26);
  for (j = 0; j < Height; j++)
  {
    for (i = 0; i < Width; i++)
    {
      EPD_WR_DATA8(0xFF);
    }
  }
    EPD_Update();
//  EPD_Update_Fast();
//  EPD_Update_Part();
}


void EPD_Clear_R26A6H(void)
{

  uint16_t i, j, Width, Height;
  Width = (EPD_W % 8 == 0) ? (EPD_W / 8) : (EPD_W / 8 + 1);
  Height = EPD_H;
  
  EPD_Init();
  EPD_WR_REG(0x26);
  for (j = 0; j < Height; j++)
  {
    for (i = 0; i < Width; i++)
    {
      EPD_WR_DATA8(0xFF);
    }
  }

  EPD_WR_REG(0xA6);
  for (j = 0; j < Height; j++)
  {
    for (i = 0; i < Width; i++)
    {
      EPD_WR_DATA8(0xFF);
    }
  }
}

void EPD_Display(const uint8_t *Image)
{
  EPD_WritePlane(0x24, Image);
  EPD_WritePlane(0x26, Image);
  EPD_Update();
}


void EPD_Display_Fast(const uint8_t *Image)
{
  EPD_WritePlane(0x24, Image);
  EPD_WritePlane(0x26, Image);
  EPD_Update_Fast();
}


void EPD_Display_Part(uint16_t x, uint16_t y, uint16_t sizex, uint16_t sizey, const uint8_t *Image)
{
  uint16_t Width, Height, i, j;
  Width = (sizex % 8 == 0) ? (sizex / 8) : (sizex / 8 + 1);
  Height = sizey;
  EPD_WR_REG(0x3C); //BorderWavefrom,
  EPD_WR_DATA8(0x80);
  EPD_WR_REG(0x21);
  EPD_WR_DATA8(0x00);
  EPD_WR_DATA8(0x00);
  EPD_WR_REG(0x3C);
  EPD_WR_DATA8(0x80);
  EPD_Address_Set(x, y, x + sizex - 1, y + sizey - 1);
  EPD_SetCursor(x, y);
  EPD_WR_REG(0x24);
  for (j = 0; j < Height; j++)
  {
    for (i = 0; i < Width; i++)
    {
      EPD_WR_DATA8(Image[i + j * Width]);
    }
  }
  EPD_Update_Part();
  // Set current and previous buffers equal so the NEXT partial diffs against
  // this frame — BOTH planes, like GxEPD2's writeImageAgain (rewriting only
  // 0x26 is not sufficient on this controller).
  EPD_Address_Set(x, y, x + sizex - 1, y + sizey - 1);
  EPD_SetCursor(x, y);
  EPD_WR_REG(0x26);
  for (j = 0; j < Height; j++)
  {
    for (i = 0; i < Width; i++)
    {
      EPD_WR_DATA8(Image[i + j * Width]);
    }
  }
  EPD_Address_Set(x, y, x + sizex - 1, y + sizey - 1);
  EPD_SetCursor(x, y);
  EPD_WR_REG(0x24);
  for (j = 0; j < Height; j++)
  {
    for (i = 0; i < Width; i++)
    {
      EPD_WR_DATA8(Image[i + j * Width]);
    }
  }
}

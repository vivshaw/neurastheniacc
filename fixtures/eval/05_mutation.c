uint32_t mutate(uint32_t x, uint32_t y) {
    uint32_t z = x ^ y;
    if (x < y) { z = z + 17u; }
    else { z = z - 17u; }
    return z * 3u;
}

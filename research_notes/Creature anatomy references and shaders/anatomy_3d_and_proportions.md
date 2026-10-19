# 3D Anatomical Models, Scans and Quantitative Proportion Data for Procedural Creatures

Scope: downloadable 3D skeleton and muscle models (with licenses), plus numeric reference data (limb ratios, segment mass and inertia, muscle models, joint range of motion, wing proportions) for tuning ornisculpt's parametric anatomy. Researched 2026-10-19. 2D drawings are out of scope.

Licence shorthand: **COMM-OK** means commercial use is allowed (CC0, CC BY, or CC BY-SA with share-alike). **NC** means non-commercial only. **REG** means registration is required.

## 1. Downloadable 3D anatomical models and scans: sources, licenses, formats

### Takeaway
For shipping in a commercial product, the only clearly safe bulk sources are **Smithsonian 3D CC0 items** and **per-model CC0/CC BY items on Sketchfab**. Z-Anatomy (human) is CC BY-SA 4.0, but some upstream parts carry NC licences. MorphoSource, oVert, Phenome10K and most museum CT data are **research/education only by default**. Use them to measure proportions, not as assets to ship.

### Cited Findings
**Smithsonian 3D (3d.si.edu)**
- Thousands of 3D objects can be downloaded, transformed and shared "for any purpose" for free. Items marked CC0 are public domain and need no attribution, permission or fee. Commercial use of non-CC0 items needs written permission. — [Smithsonian Open Access](https://www.si.edu/spotlight/openaccesshighlights)
- Downloads come as OBJ or glTF. A CC0 filter sits under "Media Usage". Over 2,000 models were available at launch in 2020 (the count is likely higher now). Highlights include a Triceratops skeleton. — [CG Channel 2020](https://www.cgchannel.com/2020/03/get-2000-free-3d-models-from-the-smithsonian-collection/); [3d.si.edu open access highlights](https://3d.si.edu/collections/openaccesshighlights)
- Sketchfab added a public-domain (CC0) dedication for cultural-heritage 3D content. — [Sketchfab blog](https://sketchfab.com/blogs/community/sketchfab-launches-public-domain-dedication-for-3d-cultural-heritage)

**Sketchfab museum accounts (licence varies per model; always check the model page)**
- NHM London blue whale skeleton: CC BY-NC-SA, so **NC**. — [Sketchfab](https://sketchfab.com/3d-models/blue-whale-skeleton-8502dbef80ed4aa688c13c90cb14de73)
- NHM Los Angeles County *Smilodon fatalis* skull: CC BY-SA with a "NoAI" tag. — [Sketchfab](https://sketchfab.com/3d-models/sabertoothed-cat-smilodon-fatalis-cbcf89e1310048e9b79c7f67f7b8b525)
- Royal Belgian Institute of Natural Sciences modern human skull: CC BY. — [Sketchfab](https://sketchfab.com/3d-models/modern-human-skull-387762294b874b5496acaabee3aa038d)
- California Academy of Sciences released 700+ scientific specimens on Sketchfab in 2018, including mammals (licences not confirmed). — [CalAcademy press release](https://www.calacademy.org/press/releases/the-academy-makes-hundreds-of-3d-scientific-specimens-available-online-for-free)
- Other natural-history accounts: Naturalis ([sketchfab.com/Naturalis](https://sketchfab.com/Naturalis)); Barcelona Natural Sciences Museum, with mounted gorilla and cormorant skeletons ([sketchfab.com/laboratorinatura](https://sketchfab.com/laboratorinatura)); CofC Natural History Museum ([sketchfab.com/CofCNaturalHistoryMuseum](https://sketchfab.com/CofCNaturalHistoryMuseum)).

**MorphoSource (CT scans; morphosource.org)**
- oVert data is "freely available for research, education and other non-commercial uses". Commercial use of MorphoSource data needs written approval from MorphoSource administrators. Commercial licensing questions go to the museum that owns the specimen. — [Florida Museum oVert copyrights](https://www.floridamuseum.ufl.edu/overt/copyrights)
- Individual records carry CC BY-NC 3.0 or CC BY-NC-SA 3.0, for example Florida Museum herpetology records. — [iDigBio media record](https://portal.idigbio.org/portal/mediarecords/384af88a-02e8-4855-8933-6f2b5019186c)
- Some records add limits beyond the CC licence: "Restricted Download", "Commercial Use Not Permitted", "3D Printing Limited", and a requirement to archive published derivatives back on MorphoSource. Example: a UChicago PaleoCT forelimb scan. — [MorphoSource media 000602892](https://n2t.net/ark:/87602/m4/602892)

**Phenome10K (phenome10k.org)**
- A free repository of CT and surface scans (STL) of biological and palaeontological specimens, "for free download by the academic and educational community". Scans are linked to publications so contributors get cited. — [Goswami lab launch post](https://www.goswamilab.com/post/2015/06/19/its-alive-the-official-launch-of-phenome10korg)

**Human: Z-Anatomy / BodyParts3D**
- The Z-Anatomy Blender template and models are CC BY-SA 4.0. — [GitHub Z-Anatomy/The-blend](https://github.com/Z-Anatomy/The-blend); [Models-of-human-anatomy](https://github.com/Z-Anatomy/Models-of-human-anatomy)
- Z-Anatomy is based on BodyParts3D (Database Center for Life Science, Japan). A third-party listing gives BodyParts3D as CC BY-SA 2.1 JP and notes that some upstream components are CC BY-NC-SA 4.0. — [SOURCES listing (third-party)](https://3d-anatomy-for-hn.duckdns.org/SOURCES.txt)
- A community mirror hosts .blend and FBX files under CC BY-SA 4.0. — [LluisV/Z-Anatomy](https://github.com/LluisV/Z-Anatomy)
- One 3D Slicer forum user decided that share-alike made the atlas unsuitable for a commercial game. This is one opinion, not legal advice. — [Slicer forum](https://discourse.slicer.org/t/open-source-human-anatomy-atlas/17734?page=2)

**Musculoskeletal simulation models (OpenSim/SIMM on SimTK)**
- John Hutchinson's (RVC) SimTK projects: `ostrich_model`, `ostrichopensim`, `greyhoundleg` (greyhound hindlimb), `velociraptor` (hindlimb), `trex2005` (T. rex hindlimb), `mousehindlimb`, `mice`. — [simtk.org/users/jhutchinson](https://www.simtk.org/users/jhutchinson)
- `trex2005` contains SIMM and OpenSim model files; an OpenSim version was added in 2019. — [simtk.org/projects/trex2005](https://simtk.org/projects/trex2005)
- The ostrich pelvic-limb model was published in PeerJ 3:e1001 (Hutchinson et al. 2015) and is listed by Stanford NMBL. — [NMBL software & models](https://nmbl.stanford.edu/software-and-models-for-the-scientific-community/)
- OstrichRL ports the ostrich OpenSim leg model to MuJoCo, with code on GitHub. It is a ready-made source of muscle origins, insertions and paths in a game-engine-friendly format. — [arXiv 2112.06061](https://arxiv.org/pdf/2112.06061)
- Nile crocodile hindlimb OpenSim model: Wiseman et al. 2021. Moment-arm Monte Carlo code is on figshare. — [figshare 14216138](https://figshare.com/articles/software/Monte_Carlo_simulation_code_Matlab_/14216138)
- Forelimb range-of-motion and moment-arm models of *Crocodylus johnstoni* and *Mussaurus* are in Otero et al. 2017 (PeerJ). — [PMC5703147](https://pmc.ncbi.nlm.nih.gov/articles/PMC5703147)
- Methods review on building musculoskeletal models of extinct animals: Bishop, Cuff & Hutchinson 2021, *Paleobiology*. — [RVC repository](https://rvc-repository.worktribe.com/output/1548818)

### Inferences
- A practical pipeline: take CC0 Smithsonian skeletons, or CC BY Sketchfab ones, for any bone meshes that ship. Use NC sources (MorphoSource, Phenome10K, NHM) only to measure ratios that become RON parameters. Numbers extracted from a scan are facts, not copyrightable assets, but check each item's terms of use.
- OpenSim `.osim` files are XML. They give body masses, inertias, joint frames and muscle origin/insertion/via points in body-local coordinates. That maps almost directly onto ornisculpt's muscle origin/insertion and mass parts. SimTK project licences vary per project and are often research-only, so check each project page.

### Gaps
- Exact licence text for each SimTK project, and whether horse or elephant OpenSim models are public, could not be confirmed. No public horse OpenSim model turned up.
- DigiMorph (digimorph.org) terms and Digital Life 3D licensing were not retrieved this session.
- Phenome10K's exact licence field was not confirmed.

## 2. Parametric animal shape models and datasets (SMAL family, Animal3D)

### Takeaway
SMAL and its relatives (SMAL+, SMALR) come from Max Planck. They are **non-commercial research licences, single user, no redistribution, and no use to train commercial models**. Good for studying silhouettes and proportions, but they cannot be embedded in a commercial tool.

### Cited Findings
- SMAL (Zuffi, Kanazawa, Jacobs, Black, CVPR 2017, "3D Menagerie") is an articulated, skinned quadruped model. It was learned from 3D scans of **toy figurines** aligned with the GLoSS method. It covers lions, tigers, horses, cows, hippos and dogs, and its training data also included cats, foxes, deer and zebras. — [smal.is.tue.mpg.de](https://smal.is.tue.mpg.de/)
- The SMAL+ licence covers "non-commercial scientific research, education, or artistic projects" only. It prohibits incorporation in a commercial product or service and using the model to train networks for commercial use. It is limited to one user with no sharing or sublicensing. MPG and IMATI-CNR hold the rights. — [awol.is.tue.mpg.de/license.html](https://awol.is.tue.mpg.de/license.html)
- SMALR (Zuffi, Kanazawa, Black, CVPR 2018) is released under the Perceiving Systems PS:License 1.0 (2018-06-20). The 3D animal meshes are downloadable and can be posed. — [is.mpg.de SMALR](https://is.mpg.de/code/smalr-capturing-animal-shape-and-texture-from-images)
- Commercial licensing for the group's models goes through ps-licensing@tue.mpg.de (stated for SMPL-X). By contrast, the human SMPL-Body is CC BY 4.0 while the SMPL model itself is patented. — [smpl.is.tue.mpg.de licence](https://smpl.is.tue.mpg.de/license.html)
- Animal3D (ICCV 2023) has 3,379 images of 40 mammal species, with 26 keypoints and SMAL pose and shape parameters per image. — [arXiv 2308.11737](https://arxiv.org/abs/2308.11737v1); [CVF open access](https://openaccess.thecvf.com/content/ICCV2023/html/Xu_Animal3D_A_Comprehensive_Dataset_of_3D_Animal_Pose_and_Shape_ICCV_2023_paper.html)

### Inferences
- SMAL's shape space can be used offline to see how proportions co-vary between, say, a dog and a hippo, and to inform ornisculpt's own parameter ranges. Do not ship SMAL meshes or blendshapes.
- Because SMAL was learned from toys, its proportions are stylised. Prefer CT- or dissection-based numbers for realism.

### Gaps
- The SMAL licence page itself was not retrieved; the terms above come from the SMAL+ (AWOL) licence. SMAL is very likely the same, but this is unverified.
- No information was found on BARC, SMALST, or VAREN (the horse model) licences.

## 3. Segment mass, centre of mass and inertia data

### Takeaway
Dissection-based body-segment parameter (BSP) tables exist for horses (Buchner 1997), German Shepherds (Jones et al. 2018, CC BY), medium dogs via MRI (Amit 2009) and humans (de Leva 1996). Each gives segment mass as a percentage of body mass, CoM as a percentage of segment length, and radii of gyration. These are exactly the values needed to tune ornisculpt's mass parts.

### Cited Findings
- **Horse:** Buchner, Savelberg, Schamhardt & Barneveld 1997, *J Biomech* 30:653, "Inertial properties of Dutch Warmblood horses". They took full 3D inertial properties (mass, density, CoM, inertia tensor) of 26 segments in 6 horses and fitted regression models to estimate them in living horses. Standard errors: 8% for segment mass, 3% of segment length for CoM, and 17% for moments of inertia. Dissection reproducibility was 2–9%. — [PubMed 9165402](https://pubmed.ncbi.nlm.nih.gov/9165402)
- A 2011 equine limb-segment study found that averages from earlier studies "generally confirmed our data but scaling relationships often needed to be revised". — [Strathclyde thesis/record](https://stax.strath.ac.uk/concern/theses/jw827b74h)
- **Dog:** Jones, Raschke & Riches 2018, *PLoS ONE*, "Inertial properties of the German Shepherd Dog" (CC BY 4.0). It tabulates normalised segment masses, CoMs, radii of gyration and densities. Each hind limb is about 6.8% of body mass, about 2.3% heavier than a forelimb. — [PMC6195294](https://www.ncbi.nlm.nih.gov/pmc/articles/PMC6195294/); [PLoS ONE article](https://journals.plos.org/plosone/article?id=10.1371/journal.pone.0206037)
- **Dog:** Amit et al. 2009, *Veterinary Journal*, measured segment mass, CoM and moments of inertia by MRI in three medium dogs of average build. — [HUJI record](https://cris.huji.ac.il/en/publications/segmental-inertial-properties-in-dogs-determined-by-magnetic-reso/)
- **Human:** de Leva 1996, *J Biomech* 29(9):1223–1230, re-references Zatsiorsky–Seluyanov's gamma-scan data to joint centres. Selected mass fractions (male/female): trunk 0.4346/0.4257, thigh 0.1416/0.1478, head 0.0694/0.0668. Thigh CoM is 0.4095/0.3612 of length from the proximal end. Head radii of gyration (male) are 0.303/0.315/0.261. — [de Leva 1996 PDF](https://ebm.ufabc.edu.br/wp-content/uploads/2013/12/Leva-1996.pdf); values via [Visual3D wiki](https://wiki.has-motion.com/Adjusted_Zatsiorsky-Seluyanov%27s_segment_inertia_parameters) (secondary)
- Open Python/Jupyter implementation of the Zatsiorsky/de Leva BSP tables: — [BMClab notebook](https://nbviewer.jupyter.org/github/demotu/BMC/blob/master/notebooks/BodySegmentParameters.ipynb)

### Inferences
- ornisculpt could store a BSP table per archetype (horse from Buchner, dog from the German Shepherd study, human from de Leva) and check generated part masses against it. For example, the sum of hind-limb part masses should be about 6.8% of body mass for a GSD-like build.

### Gaps
- The actual numeric tables from Buchner 1997 and from Jones 2018 (Table 1) could not be extracted: the PLoS tables are images and Buchner is paywalled. Pull them from the PDFs or supplementary files.
- No segment inertia data for birds, bats or felids was found this session.

## 4. Allometric scaling of limbs

### Takeaway
Across land mammals, limb bone length scales roughly as M^0.32–0.35 and diameter as M^0.36. That is close to geometric similarity (M^0.33), with Bovidae as an exception. Use this to scale bone thickness with creature size, for example so dragon-sized bodies get thicker limbs.

### Cited Findings
- Alexander, Jayes, Maloiy & Wathuta 1979, *J Zool* 189:305–314, measured 37 species from shrews to elephants. Bone length ∝ M^0.35 and diameter ∝ M^0.36, except in Bovidae. — [Hokkanen, citing the abstract](https://www.miketaylor.org.uk/dino/hokkanen/Size-Hokkanen.pdf)
- Hokkanen quotes the fitted equations as **L = 74·M^0.32** and **D = 4.8·M^0.36** (exponent 95% CI about ±0.02). This conflicts with the 0.35 length exponent in the abstract; the original fit was not checked. — [Hokkanen](https://www.miketaylor.org.uk/dino/hokkanen/Size-Hokkanen.pdf)
- Biewener 1983 (32 species): limb bone length ∝ M^0.31 and diameter ∝ M^0.35. Large mammals keep bone stress down mainly by standing more upright (posture and limb orientation) and by changes in duty factor and bone curvature. — [Biewener lab](https://biewenerlab.oeb.harvard.edu/publications/allometry-quadrupedal-locomotion-scaling-duty-factor-bone-curvature-and-limb)
- Raich & Casinos 1991 found limb length exponents close to 0.34. — [Belgian J Zool PDF](https://biblio.naturalsciences.be/associated_publications/bjz/bjz_121-2/bjz_121_raich_295-314.pdf)
- McMahon's elastic-similarity theory predicts length ∝ M^0.25 and diameter ∝ M^0.38. — [Hokkanen](https://www.miketaylor.org.uk/dino/hokkanen/Size-Hokkanen.pdf)

### Inferences
- For a size slider: limb length ≈ k·M^0.33 and diameter ≈ k'·M^0.36. The ratio D/L then grows as about M^0.03 under the empirical data, or M^0.13 under elastic similarity. Very large creatures should also get straighter, more columnar limbs (Biewener's posture effect) rather than just thicker bones.

### Gaps
- Bird and bat hindlimb allometry exponents were not gathered.

## 5. Wing proportions: birds, bats, pterosaurs (dragon plausibility)

### Takeaway
Living flying birds top out at about 3 m span and about 16–22 kg. Giant azhdarchid pterosaurs reached 10–11 m span at about 200–250 kg (Witton & Habib 2010), so a plausible giant flyer is far lighter than its size suggests. The wing finger is 47–66% of wing length in pterosaurs. Bats have aspect ratios of roughly 5–11.

### Cited Findings
**Pterosaurs / giant flyers** (Witton & Habib 2010, *PLoS ONE* 5:e13982, CC BY)
- *Quetzalcoatlus northropi*: span 10–11 m, humerus length 544 mm, preferred mass **180–250 kg** (200–250 kg at 10 m span). *Pteranodon*: span about 7 m (largest 7.25 m). Most pterodactyloid groups span 4–6 m. Non-pterodactyloids never exceed about 3 m. Flying birds max out at about 3 m. — [Witton & Habib 2010](https://journals.plos.org/plosone/article?id=10.1371/journal.pone.0013982)
- Henderson's 544 kg *Quetzalcoatlus* estimate drops to about 240 kg once torso length is corrected. Henderson modelled the torso at 1.8 m; the actual is about 0.65 m. — [same](https://journals.plos.org/plosone/article?id=10.1371/journal.pone.0013982)
- Proportions: wing finger is **66% of wing length in *Pteranodon* and 47% in *Q. northropi***. *Pteranodon* hindlimb is about 20% of wingspan. The torso is small: about 0.65 m in a 10 m azhdarchid. — [same](https://journals.plos.org/plosone/article?id=10.1371/journal.pone.0013982)
- Body density: birds 0.6–0.9 g/cm³; non-flying animals about 1 g/cm³. Pneumaticity of 60–90% is needed for 70 kg-class masses. *Anhanguera* has over 40% of mass in appendicular bone, muscle and skin. Feathers are over 10% of bird body mass. — [same](https://journals.plos.org/plosone/article?id=10.1371/journal.pone.0013982)
- Bone strength: the *Q. northropi* humerus is more than 2× the avian expectation for failure resistance (2.82× at 180 kg), but its femur is below 1×. The fifth cervical vertebra is about 2× as strong in bending as the femur. — [same](https://journals.plos.org/plosone/article?id=10.1371/journal.pone.0013982)
- Flight (*Q. northropi*, Witton planform): best glide 24.9 m/s, minimum sink 16.3 m/s, maximum range speed 48.3 m/s. CLmax is 2.2, about 33% above birds. Launch is quadrupedal: a 200–250 kg, 10 m azhdarchid could take off from standing. Living birds lack wingtip slots above aspect ratio 12. — [same](https://journals.plos.org/plosone/article?id=10.1371/journal.pone.0013982)
- Largest living flying birds: Kori bustard 16 kg; great bustard up to 22 kg. — [same](https://journals.plos.org/plosone/article?id=10.1371/journal.pone.0013982)
- Habib estimated a flight range of 8,000–12,000 miles; David Unwin was sceptical of the distance. — [NPR 2010](https://www.npr.org/2010/11/16/131362653/peerless-pterosaur-could-fly-long-distance-for-days)

**Bats**
- Aspect ratio from span²/area is typically 5–11. A second "digit-based" aspect ratio, (wrist to digit-3 tip + forearm) / (wrist to digit-5 tip), gives values that cannot be compared with the first. — [Wikipedia: Bat flight](https://en.wikipedia.org/wiki/Bat_flight)
- In Norberg & Rayner 1987 (215 species), more than 2/3 of bats have half-span aspect ratios of 2.75–3.75. High-AR bats fly in open air; low-AR bats fly in clutter. — [PMC5206602](https://pmc.ncbi.nlm.nih.gov/articles/PMC5206602)
- Bat wingspan scales about isometrically, b ∝ M^0.32 (Norberg & Rayner 1987). — [USC Dryden PDF](https://sites.usc.edu/drydenwt/files/2022/06/HJS_BB_09.pdf)
- Norberg & Rayner sort bats into foraging guilds by body mass, aspect ratio, wing loading and wingtip index. — [Ospina-Garcés et al. 2023](https://www.uv.mx/personal/cmacswiney/files/2023/11/Ospina-Garces_etal_2023.pdf)

**Birds**
- The brachial index (humerus : ulna/radius) differs between kinematic groups defined by wingbeat frequency (Nudds et al.). Primary-feather and total-arm-length ratios (humerus + ulna + manus) separate four flight-style groups. Manus length tracks wing size but is the poorest predictor of flight style. — [Wang et al. 2011, PLoS ONE, PMC3233598](https://www.ncbi.nlm.nih.gov/pmc/articles/PMC3233598/)
- Griffon vulture (a soarer) has a markedly longer ulna and higher bone laminarity than the greater flamingo (a flapper), from 10 birds per species. — [Frongia et al. 2021](https://iris.uniss.it/bitstream/11388/246676/1/Frongia%20et%20al%202021.pdf)

### Inferences
- Dragon or wyvern mass budget: for a 10 m span, aim for about 200–250 kg, a small torso, and a very large humerus and pectoral region. A 1-tonne dragon at 10 m is implausible. For pterosaur-style wings, set the wing-finger fraction at 0.47 (azhdarchid, broad wing) to 0.66 (*Pteranodon*, long narrow wing). For bat-style membranes, target an aspect ratio of about 6–8.
- ornisculpt's wing parameters could be expressed as normalised fractions: humerus, forearm and manus/finger as fractions of wing length, with brachial index as a style knob (soaring means a longer ulna).

### Gaps
- Witton & Habib's Table 1 (wing areas, aspect ratios, wing loadings) could not be extracted.
- The numeric brachial-index ranges per flight group in Wang et al. 2011 were not retrieved; PMC served a captcha.
- Bat digit-length ratios (digits III–V relative to forearm) by species were not obtained.

## 6. Joint range-of-motion data

### Takeaway
Breed-specific dog goniometry exists (Labrador, German Shepherd, Rottweiler). Bird wing ROM has been measured ex vivo across 61 species. Turkey elbow XROMM gives about 90° flexion/extension, about 50° abduction/adduction and about 40° long-axis rotation. Most per-joint tables sit behind paywalls.

### Cited Findings
- Jaegger, Marcellin-Little & Levine 2002, *Am J Vet Res* 63(7):979–986, measured flexion and extension of the carpus, elbow, shoulder, tarsus, stifle and hip in 16 Labradors, and found goniometry reliable. — [FourLeg News summary](https://www.fourleg.com/media/9%281%29FourLegNews.pdf)
- German Shepherds had lower flexion and extension than Labradors at every joint except the carpus. Breed conformation significantly affects ROM. — [Frontiers Vet Sci 2019](https://frontiersin.org/journals/veterinary-science/articles/10.3389/fvets.2019.00424/full)
- Turkey humeroulnar joint (XROMM, n=4): about 90° flexion/extension, about 50° abduction/adduction and about 40° long-axis rotation. Abduction/adduction narrows as the elbow extends. — [SICB abstract](https://sicb.org/?p=6788)
- Ex vivo 3D wing ROM in 61 species from 20 orders: ROM correlates strongly with flight behaviour and body mass, while wing shape does not. — [PMC6810231](https://pmc.ncbi.nlm.nih.gov/articles/PMC6810231)
- Ligament constraints are needed to model avian shoulder mobility correctly (partridge, XROMM). — [Soft tissue constraints on avian shoulder](https://ouci.dntb.gov.ua/en/works/7B6zWVvw/)
- Penguins have greatly reduced intrinsic wing-joint mobility; alcids and diving-petrels do not (Raikow et al., Auk 1988). — [Auk PDF](https://sora.unm.edu/sites/default/files/journals/auk/v105n03/p0446-p0451.pdf)
- Range of motion and moment arms for *Crocodylus* and *Mussaurus* forelimbs. — [Otero et al. 2017, PMC5703147](https://pmc.ncbi.nlm.nih.gov/articles/PMC5703147)

### Inferences
- Joint limits in ornisculpt should be coupled, not independent per axis. The turkey elbow loses abduction/adduction as it extends, which suggests limit cones that depend on flexion.

### Gaps
- The numeric Labrador ROM table (Jaegger 2002) and horse joint ROM tables were not retrieved.
- No muscle PCSA (physiological cross-sectional area) tables were gathered this session. The OpenSim models (ostrich, greyhound, croc, T. rex) contain max isometric force values, which are a proxy for PCSA, and are the best next source.
- No insect morphometric or 3D sources were researched. Candidates to check: micro-CT insect sets on MorphoSource or Dryad.
- No lizard limb-ratio data was found.
